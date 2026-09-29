//! An interactive console that runs the sample travel desk against a real model, from one
//! seeded trip: two legs, the outbound cancelled, and the airline's offer for it.
//!
//! The other examples script the model; this one talks to a real endpoint, which is
//! the only way to see what a model does with the contract. Each turn prints the
//! understanding's steps live as they are decided, then what was understood, then
//! what the runtime did with it. The two lists differ, and the gap is the design.
//!
//! ```text
//! OPENAI_API_KEY=…     cargo run -p console      # also ANTHROPIC_API_KEY, GEMINI_API_KEY
//! cargo run -p console                           # falls back to a local Ollama
//! ```
//!
//! `TURNFRAME_MODEL` picks the model, `OLLAMA_URL` points Ollama elsewhere,
//! `TURNFRAME_CONFIG` names a TOML file merged over the conservative configuration, and
//! `TURNFRAME_TRACE=1` writes every event and model call to the gitignored `traces/`, and
//! `TURNFRAME_LOCALE=it-IT` has the replies written in Italian. Type `/help` for the commands.

#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod directory;
mod print;
mod setup;
mod style;

use std::io::{BufRead, Write};
use std::sync::Arc;

use anyhow::Context as _;
use turnframe::effort::Effort;
use turnframe::flow::WorkflowRegistry;
use turnframe::ids::{AccountId, CaseId, CaseRevision, ConversationId, OptionId, TurnId};
use turnframe::interaction::Interaction;
use turnframe::locale::Locale;
use turnframe::provider::provider::ModelProvider;
use turnframe::provider::router::ProviderPool;
use turnframe::provider::trace::TracedProvider;
use turnframe::runtime::orchestrator::Orchestrator;
use turnframe::runtime::stream::{TurnEvent, TurnSink};
use turnframe::runtime::trace::JsonlTrace;
use turnframe::store::conversation::ConversationRecord;
use turnframe::testing::stores::FakeStores;
use turnframe::testing::workflows::InMemoryExecutor;
use turnframe::testing::workflows::traveler::{TravelerWorkflow, active_traveler};
use turnframe::testing::workflows::trip::{
    TripState, TripWorkflow, complete_case, sample_offer, sample_travel_date,
};
use turnframe::turn::{ActorContext, InteractionResponse, TurnInput};

/// The tenant every turn runs in.
const ACCOUNT: &str = "aurora";
/// The workflow key of the sample trip domain.
const TRIP: &str = "trip";
/// The workflow key of the sample traveler domain.
const TRAVELER: &str = "traveler";
/// Where `TURNFRAME_TRACE=1` writes: the repository's gitignored `traces/`.
const TRACES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../traces");

/// Prints each understanding step the moment it is decided, and the line a model wrote
/// for it when `[narration] steps = true`.
struct LiveSteps;

impl TurnSink for LiveSteps {
    fn emit(&self, event: TurnEvent) {
        match event {
            TurnEvent::Step(step) => {
                println!("  {}", style::muted(&format!("· {}", step.describe())))
            }
            TurnEvent::StepSaid { text, .. } => println!("  {}", style::said(&format!("› {text}"))),
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The repository's .env, when there is one; a variable set in the shell wins.
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    // Warnings by default, so a call a provider refuses says why; RUST_LOG overrides.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let account = AccountId::from(ACCOUNT);
    let conversation = ConversationId::nil();
    let locale =
        Locale::from(std::env::var("TURNFRAME_LOCALE").unwrap_or_else(|_| "en-GB".to_owned()));

    let (provider, description) = setup::provider_from_environment()?;
    let config = setup::config_from_environment().context("reading TURNFRAME_CONFIG")?;
    let trace = JsonlTrace::from_environment(TRACES)
        .context("opening the trace TURNFRAME_TRACE asks for")?
        .map(Arc::new);
    let provider: Arc<dyn ModelProvider> = match &trace {
        Some(trace) => Arc::new(TracedProvider::new(provider, Arc::clone(trace) as _)),
        None => provider,
    };
    // One trip is seeded, the disruption the desk is working on; the rest the conversation creates.
    let trips = Arc::new(InMemoryExecutor::new(TripWorkflow::default()));
    trips.seed(
        &account,
        &CaseId::from("trip-1"),
        seeded_trip(),
        CaseRevision(1),
    );
    let travelers = Arc::new(InMemoryExecutor::new(TravelerWorkflow::default()));
    travelers.seed(
        &account,
        &CaseId::from("trav-1"),
        active_traveler(),
        CaseRevision(1),
    );
    let records = directory::Records {
        trips: Arc::clone(&trips),
        travelers: Arc::clone(&travelers),
    };

    let stores = FakeStores::new();
    let mut builder = Orchestrator::builder()
        .workflows(Arc::new(
            WorkflowRegistry::builder()
                .register(TripWorkflow::default(), Arc::clone(&trips))
                .register(TravelerWorkflow::default(), Arc::clone(&travelers))
                .build()?,
        ))
        .providers(Arc::new(
            ProviderPool::builder().provider(provider).build()?,
        ))
        .stores(stores.stores().clone())
        .case_directory(Arc::new(records.clone()))
        .config(config);
    if let Some(trace) = &trace {
        builder = builder.trace(Arc::clone(trace) as _);
    }
    let orchestrator = builder.build()?;
    stores
        .stores()
        .conversations()
        .create_conversation(ConversationRecord::new(
            conversation,
            account.clone(),
            stores.now(),
        ))
        .await?;

    println!(
        "\n  {}",
        style::heading(&format!("Turnframe console: {description}"))
    );
    println!(
        "  {}",
        style::muted(
            "Workflows: the sample travel desk, in memory. Trip 1 is for Marta Bianchi: \
             the outbound is cancelled and the airline quotes AZ612 at 13:10.",
        )
    );
    if let Some(trace) = &trace {
        let path = std::fs::canonicalize(trace.path()).unwrap_or_else(|_| trace.path().into());
        println!(
            "  {}",
            style::muted(&format!(
                "Saving this session's traces to {}",
                path.display()
            ))
        );
    }
    println!(
        "  {}\n",
        style::muted("Bright white is what a user would see; grey is how the turn got there.")
    );
    println!(
        "  {}\n",
        style::muted("/help for commands, /quit to leave.")
    );
    print::state(&records, &account).await;

    let sink: Arc<dyn TurnSink> = Arc::new(LiveSteps);
    let mut lines = std::io::stdin().lock().lines();
    let mut turn_number: u128 = 1;
    let mut card: Option<Interaction> = None;
    let mut effort = Effort::Medium;
    loop {
        print!("{} ", style::prompt());
        std::io::stdout().flush()?;
        let Some(line) = lines.next() else {
            break;
        };
        let line = line?;
        let text = line.trim();
        match text {
            "" => continue,
            "/quit" | "/exit" => break,
            "/help" => {
                print::help();
                continue;
            }
            "/state" => {
                print::state(&records, &account).await;
                continue;
            }
            command if command.starts_with("/effort") => {
                match command
                    .trim_start_matches("/effort")
                    .trim()
                    .parse::<Effort>()
                {
                    Ok(level) => {
                        effort = level;
                        println!("  {}", style::muted(&format!("Effort: {level}.")));
                    }
                    Err(error) => println!("  {}", style::warning(&error.to_string())),
                }
                continue;
            }
            _ => {}
        }

        let turn_id = TurnId::from(uuid::Uuid::from_u128(turn_number));
        turn_number += 1;
        let mut input = TurnInput {
            turn_id,
            conversation_id: conversation,
            actor: ActorContext::new(account.clone(), "u-1"),
            text: Some(text.to_owned()),
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: locale.clone(),
            effort: Some(effort),
        };
        // A number while a card is on screen presses that button: no model call.
        if let Some((card, option)) = card.as_ref().zip(pressed(text, card.as_ref())) {
            input.text = None;
            input.interaction_response = Some(InteractionResponse {
                interaction_id: card.id,
                option_id: option,
                expected_case_revision: records.revision_of(&account, &card.case_ref.key()),
                freeform_input: None,
            });
        }

        println!();
        match orchestrator
            .handle_turn_streaming(input, Arc::clone(&sink))
            .await
        {
            Ok(turn) => {
                if let Ok(record) = stores.stores().replay().get(&account, &turn_id).await {
                    print::understood(&record, text);
                }
                print::turn(&turn, &locale);
                card = open_card(&stores, &records, &account).await;
                if let Some(card) = card.as_ref() {
                    print::card(card, &locale);
                }
            }
            // A refused turn is an outcome, and printing it is the point.
            Err(error) => println!(
                "\n  {}\n",
                style::warning(&format!("the turn was refused: {error}"))
            ),
        }
    }
    Ok(())
}

/// The option a bare number stands for, when a card is on screen.
fn pressed(text: &str, card: Option<&Interaction>) -> Option<OptionId> {
    let index: usize = text.parse().ok()?;
    card?
        .payload
        .options
        .get(index.checked_sub(1)?)
        .map(|option| option.id.clone())
}

/// The blocking card waiting on a record, if the turn left one.
async fn open_card(
    stores: &FakeStores,
    records: &directory::Records,
    account: &AccountId,
) -> Option<Interaction> {
    for (key, _) in records.labelled(account) {
        let open = stores.open_interactions(account, &key).await.ok()?;
        if let Some(card) = open.into_iter().find(|interaction| interaction.blocking) {
            return Some(card);
        }
    }
    None
}

/// The trip the desk is working on: complete, with the airline's offer for the cancelled
/// outbound, so a rebooking can be asked for at once.
fn seeded_trip() -> TripState {
    TripState {
        name: Some("Lisbon offsite".to_owned()),
        travel_date: Some(sample_travel_date()),
        offer: Some(sample_offer(1)),
        ..TripState {
            extras: Vec::new(),
            ..complete_case()
        }
    }
}
