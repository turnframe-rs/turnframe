//! A mixed turn: a click on the rebooking card and, in the same message, a
//! prohibition, a field change and a question.
//!
//! The user clicks "Confirm" and writes «Actually, do not rebook anything yet.
//! While you are there, change the name to Lisbon, October, and tell me what the
//! fare difference is.» The whole turn is reduced before anything runs, so the prohibition
//! reaches the rebooking the click authorized before that rebooking reaches the
//! domain: nothing is rebooked and a notice says so, the name change commits
//! with a receipt citing its event, the question is answered on a stated basis,
//! and the replay record accounts for each act.
//!
//! Understanding is scripted with [`ScriptedUnderstanding`] and narration with a
//! [`ScriptedProvider`], so the program needs no key and no network. A real
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

use turnframe::flow::{CaseKey, CaseRef, WorkflowRegistry};
use turnframe::ids::{AccountId, CaseId, CaseRevision, ConversationId, TargetToken, TurnId};
use turnframe::interaction::Interaction;
use turnframe::locale::Locale;
use turnframe::provider::provider::ModelProvider;
use turnframe::provider::purpose::ModelPurpose;
use turnframe::provider::router::ProviderPool;
use turnframe::provider::trace::TracedProvider;
use turnframe::response::{AssistantTurn, ResponseBlock};
use turnframe::runtime::orchestrator::{CaseCandidate, Orchestrator, StaticCaseDirectory};
use turnframe::runtime::reduce::notice;
use turnframe::runtime::resolve::{AuthorizedCase, TargetResolver};
use turnframe::runtime::resume;
use turnframe::runtime::trace::JsonlTrace;
use turnframe::store::conversation::ConversationRecord;
use turnframe::testing::providers::{
    ScriptedProvider, ScriptedUnderstanding, UnderstandingBuilder,
};
use turnframe::testing::stores::FakeStores;
use turnframe::testing::workflows::InMemoryExecutor;
use turnframe::testing::workflows::trip::{
    REBOOK_CONFIRM_OPTION, TripWorkflow, operations, with_offer,
};
use turnframe::turn::{ActorContext, InteractionResponse, TurnInput};
use turnframe::understand::TurnUnderstander;
use turnframe::understanding::{ActAction, ConstraintKind, Understanding};

/// The tenant every turn in this example runs in.
/// Where `TURNFRAME_TRACE=1` writes: the repository's gitignored `traces/`.
const TRACES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../traces");

const ACCOUNT: &str = "aurora";

/// The workflow key of the sample trip domain.
const TRIP: &str = "trip";

/// The one case this example works on.
const CASE: &str = "trip-1";

/// The server-authored label understanding sees instead of the case identifier.
const LABEL: &str = "Trip 1";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let account = AccountId::from(ACCOUNT);
    let conversation = ConversationId::nil();
    let case_key = CaseKey::new(TRIP, CASE);

    let trips = Arc::new(InMemoryExecutor::new(TripWorkflow::default()));
    trips.seed(
        &account,
        &CaseId::from(CASE),
        with_offer(1),
        CaseRevision(3),
    );

    let setup = TurnId::from(uuid::Uuid::from_u128(1));
    let mixed = TurnId::from(uuid::Uuid::from_u128(2));

    // Turn 1 only exists so that turn 2 has a real card to answer.
    let setup_text = "It is ready, show me the rebooking card";
    let setup_understood = UnderstandingBuilder::of(setup_text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(&account, setup),
            serde_json::json!({ "leg": 1 }),
            "show me the rebooking card",
        )
        .build()?;

    // Turn 2: the acceptance case. Understanding reads only the text; the click
    // joins the turn from the card's stored option.
    let mixed_text = "Actually, do not rebook anything yet. While you are there, change the \
                      name to Lisbon, October, and tell me what the fare difference is.";
    let mixed_understood = UnderstandingBuilder::of(mixed_text)
        .constrain(ConstraintKind::DoNotSubmit, "do not rebook anything yet.")
        .apply(
            operations::SET_NAME,
            token_for(&account, mixed),
            serde_json::json!({ "value": "Lisbon, October" }),
            "change the name to Lisbon, October,",
        )
        .ask("tell me what the fare difference is.")
        .build()?;

    let understander = Arc::new(
        ScriptedUnderstanding::new()
            .then(setup_understood)
            .then(mixed_understood.clone()),
    );
    // The model writes prose and nothing else: one narration per turn, and the
    // answer to the question.
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Right, here is where that stands.")
        .answering("The new flight costs €84.00 more.")
        .acknowledging("Understood, I have not sent the rebooking.")
        .build_shared();

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
                .register(TripWorkflow::default(), Arc::clone(&trips))
                .build()?,
        ))
        .providers(Arc::new(ProviderPool::builder().provider(traced).build()?))
        .understander(Arc::clone(&understander) as Arc<dyn TurnUnderstander>)
        .stores(stores.stores().clone())
        .case_directory(Arc::new(
            StaticCaseDirectory::new().with_case(CaseCandidate::new(case_key.clone(), LABEL)),
        ));
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
        "One message, four things, four answers",
        "A card confirmation, a field change, a question and a prohibition — two of\n\
         which contradict each other — in a single turn.",
    );

    // -------------------------------------------------------------------
    section("1. Setting the scene: a real send confirmation");
    println!("  user  {setup_text:?}\n");
    let turn = orchestrator
        .handle_turn(text_turn(&account, conversation, setup, setup_text))
        .await?;
    print_turn(&turn);
    let card = blocking_card(&stores, &account, &case_key).await;
    let locale = Locale::from("en-GB");
    println!(
        "  card {}: {:?}",
        card.id,
        card.payload.title.resolve(&locale)
    );
    for option in &card.payload.options {
        println!(
            "    {:<8} {:?} -> {:?}",
            option.id.as_str(),
            option.label.resolve(&locale),
            option.action
        );
    }
    println!(
        "\n  The {REBOOK_CONFIRM_OPTION:?} option means \"apply trip.rebook\", and it means\n\
         \x20 that on the server. That is the option the next turn clicks."
    );

    // -------------------------------------------------------------------
    section("2. The turn");
    let revision = trips.revision_of(&account, &CaseId::from(CASE));
    println!(
        "  user  [clicks {REBOOK_CONFIRM_OPTION:?} on card {}]",
        card.id
    );
    println!("        expected_case_revision: {revision}");
    println!("        {mixed_text:?}\n");
    println!("  understanding read, from the text alone:");
    print_understanding(mixed_text, &mixed_understood);
    println!("  and the runtime added, from the click alone:");
    println!(
        "    {:<10} {} <- the card's stored option, not the message\n",
        resume::card_act_id().to_string(),
        operations::REBOOK
    );

    let turn = orchestrator
        .handle_turn(TurnInput {
            interaction_response: Some(InteractionResponse {
                interaction_id: card.id,
                option_id: REBOOK_CONFIRM_OPTION.into(),
                expected_case_revision: revision,
                freeform_input: None,
            }),
            ..text_turn(&account, conversation, mixed, mixed_text)
        })
        .await?;

    // -------------------------------------------------------------------
    section("3. The reply, block by block");
    print_turn(&turn);

    // -------------------------------------------------------------------
    section("4. Four things asked, four results");
    let events = stores.event_types(&account, &case_key).await?;
    let submitted = events.iter().any(|event| event == "trip.rebooking_sent");
    let name_receipt = turn
        .receipts()
        .find(|receipt| receipt.status_code == "trip.name_set");
    let nothing_submitted = turn.blocks.iter().find_map(|block| match block {
        ResponseBlock::Notice(n) if n.code == notice::NOTHING_SUBMITTED => Some(n),
        _ => None,
    });
    let answer = turn.blocks.iter().find_map(|block| match block {
        ResponseBlock::Answer(answer) => Some(answer),
        _ => None,
    });

    result(
        "the click: rebook this trip",
        &match (&nothing_submitted, submitted) {
            (Some(n), false) => format!(
                "refused, and said so: notice {} ({:?})",
                n.code,
                n.text.resolve(&locale)
            ),
            (_, true) => "REBOOKED: this example would be broken".to_owned(),
            (None, false) => "not rebooked, but no notice was raised".to_owned(),
        },
    );
    result(
        "\"do not rebook anything yet\"",
        "applied: it is the reason the click above did not rebook",
    );
    result(
        "\"change the name to Lisbon, October\"",
        &match name_receipt {
            Some(receipt) => format!(
                "committed: receipt {} citing {} event(s)",
                receipt.status_code,
                receipt.event_ids.len()
            ),
            None => "no receipt: this example would be broken".to_owned(),
        },
    );
    result(
        "\"tell me what the fare difference is\"",
        &match answer {
            Some(answer) => format!(
                "{:?} on basis {:?}: {:?}",
                answer.status, answer.basis, answer.text
            ),
            None => "the question disappeared: this example would be broken".to_owned(),
        },
    );
    println!(
        "\n  Nothing disappeared, and nothing was executed \"as far as it got\". The\n\
         \x20 whole turn was reduced first, so the prohibition reached the rebooking\n\
         \x20 before the rebooking reached the domain."
    );

    // -------------------------------------------------------------------
    section("5. What actually landed");
    println!("  event ledger of {TRIP}/{CASE}, in append order:");
    for (position, event_type) in events.iter().enumerate() {
        println!("    {}. {event_type}", position + 1);
    }
    println!(
        "\n  trip.rebooking_sent present: {submitted}\n\
         \x20 stored name:           {:?}",
        turnframe::WorkflowExecutor::load(trips.as_ref(), &account, &CaseId::from(CASE))
            .await?
            .value
            .and_then(|state| state.name)
    );
    println!("  journal entries of this turn:");
    for entry in stores.journal_for_turn(&account, &mixed).await? {
        println!("    {:<24} {:?}", entry.command_type, entry.status);
    }
    println!(
        "  The rebooking was compiled and judged — section 6 prints the decision —\n\
         \x20 but it was never admitted to the journal, because a command is only\n\
         \x20 journaled once it may run or once a card is waiting to authorize it.\n\
         \x20 There is nothing here to resume and nothing to reconcile."
    );

    // -------------------------------------------------------------------
    section("6. What the audit trail kept");
    let record = stores.replay_record(&account, &mixed).await?;
    println!("  phase              {:?}", record.phase);
    println!(
        "  workflow versions  {:?}",
        record
            .workflow_versions
            .iter()
            .map(|version| format!("{}@{}", version.key, version.version))
            .collect::<Vec<_>>()
    );
    let reduced = record
        .understanding
        .as_ref()
        .expect("a turn with text records what it reduced");
    assert!(
        reduced.act(resume::card_act_id()).is_some(),
        "the record keeps the click's act beside the message's"
    );
    println!(
        "  understanding      acts {:?}, {} question(s), {} constraint(s)",
        reduced
            .acts
            .iter()
            .map(|act| act.id.to_string())
            .collect::<Vec<_>>(),
        reduced.questions.len(),
        reduced.constraints.len()
    );
    println!(
        "                     (what was reduced: the message's reading, and the\n\
         \x20                     click's act {} taken from the card's stored option)",
        resume::card_act_id()
    );
    for resolution in &record.target_resolutions {
        println!(
            "  target of {:<8} {}",
            resolution.act.to_string(),
            short(&resolution.resolution)
        );
    }
    for decision in &record.policy_decisions {
        println!(
            "  policy             allowed: {:<5} risk: {:?} confirmation: {:?} ({})",
            decision.allowed,
            decision.policy.risk,
            decision.policy.confirmation,
            decision.reason_key
        );
    }
    for outcome in &record.command_outcomes {
        println!(
            "  command outcome    {:?} on {} @ {}",
            outcome.outcome, outcome.case_ref.case_id, outcome.case_ref.expected_revision
        );
    }
    println!("  events             {}", record.event_ids.len());
    println!("  response blocks    {}", record.response_block_ids.len());

    provider.verify()?;
    assert!(
        provider.calls().iter().all(|call| matches!(
            call.purpose(),
            ModelPurpose::Acknowledge | ModelPurpose::Answer | ModelPurpose::Review
        )),
        "the provider is asked for prose only"
    );
    assert_eq!(
        understander.seen().len(),
        2,
        "one understanding per message"
    );
    assert_eq!(
        understander.remaining(),
        0,
        "every scripted message was used"
    );
    println!(
        "\n  The scripted provider was followed exactly: two acknowledgements, each\n\
         \x20 reviewed, and one answer, and nothing else. Each message was understood\n\
         \x20 once; the click was not understood at all, because it carries its own\n\
         \x20 meaning.\n"
    );
    Ok(())
}

/// A short label for a target resolution, so the audit line stays one line.
fn short(resolution: &turnframe::turn::target::TargetResolution) -> String {
    match resolution {
        turnframe::turn::target::TargetResolution::Exact { case_ref } => {
            format!(
                "Exact({} @ {})",
                case_ref.case_id, case_ref.expected_revision
            )
        }
        turnframe::turn::target::TargetResolution::Ambiguous { candidates } => {
            format!("Ambiguous({} candidates)", candidates.len())
        }
        other => format!("{other:?}"),
    }
}

/// Each act, question and constraint of an understanding, beside the words of
/// `text` it points at.
fn print_understanding(text: &str, understanding: &Understanding) {
    let words = |range: turnframe::understanding::WordRange| {
        text.get(range.start..range.end).unwrap_or_default()
    };
    for act in &understanding.acts {
        let action = match &act.action {
            ActAction::Apply { operation } => operation.to_string(),
            ActAction::Start { workflow } => format!("start {workflow}"),
        };
        println!(
            "    {:<10} {action} <- {:?}",
            act.id.to_string(),
            words(act.words)
        );
    }
    for question in &understanding.questions {
        println!(
            "    question   on {:?} <- {:?}",
            question.basis,
            words(question.words)
        );
    }
    for constraint in &understanding.constraints {
        println!(
            "    constraint {:?} <- {:?}",
            constraint.kind,
            words(constraint.words)
        );
    }
}

/// The opaque token the runtime issues for the case in this turn.
fn token_for(account: &AccountId, turn_id: TurnId) -> TargetToken {
    TargetResolver::builder(account.clone(), turn_id)
        .candidate(AuthorizedCase::new(
            CaseRef::new(TRIP, CASE, CaseRevision::ZERO),
            LABEL,
        ))
        .build()
        .token_map()
        .token_for(&CaseKey::new(TRIP, CASE))
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

async fn blocking_card(stores: &FakeStores, account: &AccountId, case: &CaseKey) -> Interaction {
    stores
        .open_interactions(account, case)
        .await
        .expect("the store answers")
        .into_iter()
        .find(|card| card.blocking)
        .expect("the case has a blocking card")
}

fn print_turn(turn: &AssistantTurn) {
    let locale = Locale::from("en-GB");
    if turn.blocks.is_empty() {
        println!("  reply         (no blocks)\n");
        return;
    }
    for block in &turn.blocks {
        match block {
            ResponseBlock::Receipt(receipt) => println!(
                "    receipt      {:<30} {:?}",
                receipt.receipt.status_code,
                receipt.receipt.title.resolve(&locale)
            ),
            ResponseBlock::Notice(n) => println!(
                "    notice       {:<30} {:?}",
                n.code,
                n.text.resolve(&locale)
            ),
            ResponseBlock::Interaction(card) => println!(
                "    card         {:<30} {:?}",
                format!("{:?}", card.view.kind),
                card.view.title.resolve(&locale)
            ),
            ResponseBlock::Answer(answer) => println!(
                "    answer       {:<30} {:?}",
                format!("{:?}", answer.status),
                answer.text
            ),
            ResponseBlock::Transition(transition) => {
                println!(
                    "    transition   {:<30} {:?}",
                    "(model-authored)", transition.text
                );
            }
            ResponseBlock::Artifact(view) => println!(
                "    artifact     {:<30} {:?}",
                view.artifact.kind,
                view.artifact.label.resolve(&locale)
            ),
            other => println!("    block        {other:?}"),
        }
    }
    println!();
}

fn result(asked: &str, outcome: &str) {
    println!("  {asked}");
    println!("      -> {outcome}");
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
