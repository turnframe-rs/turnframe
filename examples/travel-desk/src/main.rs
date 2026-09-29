//! The travel-disruption desk: four guarantees, one after another.
//!
//! A traveler's outbound flight is cancelled. Turns run against the sample travel domain and
//! the in-memory stores: one message registers the traveler, puts her on the trip and adds an
//! extra, the trip's act waiting within the turn for the traveler it needs; one message rebooks
//! the outbound, keeps the return and forbids confirming; a misread that would change the return
//! is held by the turn's own condition, and the lock refuses any writer, the airline too; the
//! airline re-quotes, so the click on the old card is stale and the next card shows the new
//! fare; the confirmed rebooking goes out and the airline does not answer, so nothing is
//! claimed; its late answer, delivered twice, is recorded once. Narration is off, so every
//! printed line is the server's.
//!
//! Understanding is scripted with [`ScriptedUnderstanding`], so the program needs no key and no
//! network. A real deployment omits `.understander(..)`.

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use turnframe::command::{
    AtomicityScope, CommandBatch, CommandEnvelope, CommandOrigin, IdempotencyKey,
};
use turnframe::error::ExecutionError;
use turnframe::event::{ExternalStatus, OutboxEntry};
use turnframe::flow::{CaseKey, CaseRef, WorkflowExecutor, WorkflowRegistry};
use turnframe::ids::{
    AccountId, BatchId, CaseId, CaseRevision, CommandId, ConversationId, TargetToken, TurnId,
};
use turnframe::interaction::Interaction;
use turnframe::locale::Locale;
use turnframe::provider::router::ProviderPool;
use turnframe::response::{AssistantTurn, ResponseBlock};
use turnframe::runtime::config::{NarrationConfig, OrchestratorConfig};
use turnframe::runtime::dispatch::{
    DispatchConfig, Dispatched, OutboxDispatcher, OutboxReconciler, OutboxSender, Reconciled,
};
use turnframe::runtime::orchestrator::{CaseCandidate, Orchestrator, StaticCaseDirectory};
use turnframe::runtime::resolve::{AuthorizedCase, TargetResolver};
use turnframe::store::conversation::ConversationRecord;
use turnframe::store::outbox::{OutboxRecord, OutboxStore};
use turnframe::testing::providers::{
    ScriptedProvider, ScriptedUnderstanding, UnderstandingBuilder,
};
use turnframe::testing::stores::FakeStores;
use turnframe::testing::workflows::InMemoryExecutor;
use turnframe::testing::workflows::traveler::{TravelerWorkflow, operations as traveler_ops};
use turnframe::testing::workflows::trip::{
    REBOOK_CONFIRM_OPTION, SAMPLE_TICKET_NUMBER, TripCommand, TripState, TripWorkflow, operations,
    sample_legs,
};
use turnframe::turn::{ActorContext, InteractionResponse, TurnInput};
use turnframe::understand::TurnUnderstander;
use turnframe::understanding::{ActId, ConstraintKind, NotUnderstoodReason, RecordValue, UnitId};

const ACCOUNT: &str = "aurora";
const TRIP: &str = "trip";
const CASE: &str = "trip-1";
const LABEL: &str = "Trip 1";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let account = AccountId::from(ACCOUNT);
    let conversation = ConversationId::nil();
    let case_key = CaseKey::new(TRIP, CASE);
    let turn = |n: u128| TurnId::from(uuid::Uuid::from_u128(n));

    // ---- the trip the disruption opened: two legs, the outbound cancelled ----
    let trips = Arc::new(InMemoryExecutor::new(TripWorkflow::default()));
    trips.seed(
        &account,
        &CaseId::from(CASE),
        opened_by_the_disruption(),
        CaseRevision(1),
    );
    let travelers = Arc::new(InMemoryExecutor::new(TravelerWorkflow::new()));

    // ---- what each message is understood to say -----------------------------
    let register = "Register Marta Bianchi, put her on this trip and add a checked bag at 40 euros";
    let registering = UnderstandingBuilder::of(register).open(
        traveler_ops::CREATE_DRAFT,
        "traveler",
        serde_json::json!({ "full_name": "Marta Bianchi" }),
        "Register Marta Bianchi",
    );
    let registered = registering.last_act().expect("the registration is an act");
    let register_understood = registering
        .apply(
            operations::SET_TRAVELER,
            token_for(&account, turn(1)),
            serde_json::json!({}),
            "put her on this trip",
        )
        .with_record("traveler", RecordValue::SameTurn { act: registered })
        .apply(
            operations::ADD_EXTRA,
            token_for(&account, turn(1)),
            serde_json::json!({ "description": "Checked bag", "quantity": 1,
                                "unit_price": { "minor": 4_000, "currency": "EUR" } }),
            "add a checked bag at 40 euros",
        )
        .build()?;

    let fill =
        "Call it Lisbon offsite, I would rather fly on 5 October, and the airline pays for the bag";
    let fill_understood = UnderstandingBuilder::of(fill)
        .apply(
            operations::SET_NAME,
            token_for(&account, turn(2)),
            serde_json::json!({ "value": "Lisbon offsite" }),
            "Call it Lisbon offsite",
        )
        .apply(
            operations::SET_TRAVEL_DATE,
            token_for(&account, turn(2)),
            serde_json::json!({ "value": "2026-10-05" }),
            "I would rather fly on 5 October",
        )
        .apply(
            operations::ASSIGN_PAYER,
            token_for(&account, turn(2)),
            serde_json::json!({ "extra": 1, "payer": "airline" }),
            "the airline pays for the bag",
        )
        .build()?;

    let protect = "Rebook the outbound on the flight the airline offered, but don't touch the \
                   return, and don't confirm anything yet";
    let protect_understood = UnderstandingBuilder::of(protect)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(&account, turn(3)),
            serde_json::json!({ "leg": 1 }),
            "Rebook the outbound on the flight the airline offered",
        )
        .apply(
            operations::PROTECT_LEG,
            token_for(&account, turn(3)),
            serde_json::json!({ "leg": 2 }),
            "don't touch the return",
        )
        .constrain(ConstraintKind::DoNotSubmit, "don't confirm anything yet")
        .build()?;

    // A reading that got the legs the wrong way round, as the turn's own check leaves it: the
    // act that would rebook the return is held, and only the condition remains.
    let misread = "rebook the flight out, keep the one back as it is";
    let misread_understood = UnderstandingBuilder::of(misread)
        .not_understood(
            NotUnderstoodReason::KeptUnchanged {
                constraint: UnitId(2),
            },
            "rebook the flight out",
        )
        .constrain(ConstraintKind::KeepUnchanged, "keep the one back as it is")
        .build()?;

    let again = "where does it stand?";
    let again_understood = UnderstandingBuilder::of(again).build()?;

    let understander = Arc::new(
        ScriptedUnderstanding::new()
            .then(register_understood)
            .then(fill_understood)
            .then(protect_understood)
            .then(misread_understood)
            .then(again_understood),
    );
    // Narration off: the provider is here to prove that no model was called.
    let provider = ScriptedProvider::builder("scripted", "model-1").build_shared();

    // ---- the runtime ---------------------------------------------------------
    let stores = FakeStores::new();
    let orchestrator = Orchestrator::builder()
        .workflows(Arc::new(
            WorkflowRegistry::builder()
                .register(TripWorkflow::default(), Arc::clone(&trips))
                .register(TravelerWorkflow::new(), Arc::clone(&travelers))
                .build()?,
        ))
        .providers(Arc::new(
            ProviderPool::builder()
                .provider(Arc::clone(&provider) as _)
                .build()?,
        ))
        .understander(Arc::clone(&understander) as Arc<dyn TurnUnderstander>)
        .stores(stores.stores().clone())
        .case_directory(Arc::new(
            StaticCaseDirectory::new().with_case(CaseCandidate::new(case_key.clone(), LABEL)),
        ))
        .config(quiet_config())
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
    let send = |id: TurnId, text: &str| text_turn(&account, conversation, id, text);

    banner(
        "Travel desk",
        "A cancelled flight, a traveler who is not registered yet, and an airline that\n\
         re-quotes, answers late or not at all.",
    );

    // -------------------------------------------------------------------------
    section("1. Dependent acts: register her, put her on the trip, add a bag");
    println!("  user  {register:?}\n");
    let reply = orchestrator.handle_turn(send(turn(1), register)).await?;
    print_turn(&reply);
    let trip = load(&trips, &account).await;
    println!(
        "  The traveler did not exist when the message arrived: putting her on the trip\n\
         \x20 waited, within the turn, for the act that registers her. The trip is now for\n\
         \x20 {:?}.",
        trip.traveler
            .as_ref()
            .map(|traveler| traveler.display_name.as_str())
    );

    section("2. Three open fields, one message");
    println!("  user  {fill:?}\n");
    let reply = orchestrator.handle_turn(send(turn(2), fill)).await?;
    print_turn(&reply);

    // -------------------------------------------------------------------------
    section("3. The airline quotes a new flight for the outbound");
    trips
        .execute(outside(&trips, &account, "quote-1", requote(8_400)))
        .await?;
    println!("  airline  AZ612 on 2026-10-05 at 13:10, 84.00 EUR more (an outside change)");
    print_legs(&load(&trips, &account).await);

    // -------------------------------------------------------------------------
    section("4. A protected leg: rebook one, keep the other, confirm nothing");
    println!("  user  {protect:?}\n");
    let reply = orchestrator.handle_turn(send(turn(3), protect)).await?;
    print_turn(&reply);
    print_legs(&load(&trips, &account).await);
    let shown = blocking_card(&stores, &account, &case_key).await;
    print_card(&shown);

    section("5. A misread of the legs, held by the turn's condition");
    println!("  user  {misread:?}\n");
    let reply = orchestrator.handle_turn(send(turn(4), misread)).await?;
    print_turn(&reply);
    assert!(notice_codes(&reply).contains(&"turnframe.notice.kept_unchanged".to_owned()));

    section("6. The lock holds against every writer, the airline included");
    println!("  airline  a new time for the return, AZ613 on 2026-10-10 at 09:30\n");
    match trips
        .execute(outside(&trips, &account, "quote-return", requote_return()))
        .await
    {
        Ok(_) => panic!("a kept leg takes no quote"),
        Err(ExecutionError::Rejected(refused)) => println!(
            "  refused  {}: {}\n",
            refused.code,
            refused
                .explanation
                .as_ref()
                .map(|text| text.resolve(&Locale::from("en-GB")).to_owned())
                .unwrap_or_default()
        ),
        Err(other) => panic!("the domain refuses, it does not fail: {other}"),
    }
    println!(
        "  The condition in the message lasts one turn; the lock lasts as long as the\n\
         \x20 trip, and nothing that writes to it can change the return."
    );

    // -------------------------------------------------------------------------
    section("7. A stale card: the airline re-quotes before the click");
    let bound = shown.case_ref.expected_revision;
    trips
        .execute(outside(&trips, &account, "quote-2", requote(13_200)))
        .await?;
    let now = trips.revision_of(&account, &CaseId::from(CASE));
    println!("  airline  the same flight, now 132.00 EUR more: the trip moves to revision {now}");
    println!(
        "  user     [clicks {REBOOK_CONFIRM_OPTION:?} on the card shown at revision {bound}]\n"
    );
    match orchestrator
        .handle_turn(click(&account, conversation, turn(6), &shown))
        .await
    {
        Ok(_) => panic!("a click on a fare that no longer holds must confirm nothing"),
        Err(refused) => println!("  refused  {refused}\n"),
    }
    assert!(
        !events(&stores, &account, &case_key)
            .await
            .iter()
            .any(|e| e == "trip.rebooking_sent")
    );
    println!("  user  {again:?}\n");
    let reply = orchestrator.handle_turn(send(turn(7), again)).await?;
    print_turn(&reply);
    let redrawn = blocking_card(&stores, &account, &case_key).await;
    print_card(&redrawn);

    // -------------------------------------------------------------------------
    section("8. Confirmed: the rebooking goes to the airline");
    println!("  user  [clicks {REBOOK_CONFIRM_OPTION:?} on the card at revision {now}]\n");
    let reply = orchestrator
        .handle_turn(click(&account, conversation, turn(8), &redrawn))
        .await?;
    print_turn(&reply);

    section("9. The airline does not answer");
    let dispatcher = OutboxDispatcher::new(
        Arc::clone(stores.memory()) as Arc<dyn OutboxStore>,
        Arc::new(SilentAirline),
        DispatchConfig::new("desk").with_send_timeout(Duration::from_millis(20)),
    );
    dispatcher.run_once(stores.now()).await?;
    let journal = stores.journal_for_turn(&account, &turn(8)).await?;
    let row = stores
        .outbox_for_command(&journal[0].command_id)
        .await?
        .remove(0);
    println!("  outbox row      {:?}", row.entry.status);
    println!(
        "  trip status     {:?}",
        load(&trips, &account).await.status
    );
    println!(
        "\n  The request left, so nobody can say it did not land: the outcome is unknown,\n\
         \x20 never retried blindly, and the reply above said sent, not confirmed."
    );

    section("10. The late answer, delivered twice, recorded once");
    let settled = dispatcher
        .reconcile(&row.entry.outbox_id, &AirlineHasIt, stores.now())
        .await?;
    println!("  reconciled      {settled:?}");
    let answer = outside(
        &trips,
        &account,
        "answer-1",
        TripCommand::RecordAirlineOutcome {
            status: ExternalStatus::Accepted,
            ticket_number: Some(SAMPLE_TICKET_NUMBER.to_owned()),
            reason_code: None,
        },
    );
    let first = trips.execute(answer.clone()).await?;
    let second = trips.execute(answer).await?;
    println!(
        "  first delivery  {} event(s), replay: {}",
        first.events.len(),
        first.idempotency_replay
    );
    println!(
        "  second delivery {} event(s), replay: {}",
        second.events.len(),
        second.idempotency_replay
    );
    let trip = load(&trips, &account).await;
    println!(
        "  trip status     {:?}, ticket {:?}\n",
        trip.status, trip.ticket_number
    );
    assert!(
        second.idempotency_replay,
        "the second delivery repeats nothing"
    );

    provider.verify()?;
    assert_eq!(provider.call_count(), 0, "no model was called");
    assert_eq!(
        understander.remaining(),
        0,
        "every scripted message was used"
    );
    Ok(())
}

/// The case the disruption opened: two legs, the outbound cancelled, no traveler yet.
fn opened_by_the_disruption() -> TripState {
    TripState {
        legs: sample_legs(),
        ..TripState::default()
    }
}

/// The airline's quote for the outbound at `fare_difference_cents`.
fn requote(fare_difference_cents: i64) -> TripCommand {
    TripCommand::Requote {
        leg: 1,
        flight: "AZ612".to_owned(),
        departs: "2026-10-05 13:10".to_owned(),
        fare_difference_cents,
    }
}

/// The airline's quote for the kept return, which the lock refuses.
fn requote_return() -> TripCommand {
    TripCommand::Requote {
        leg: 2,
        flight: "AZ613".to_owned(),
        departs: "2026-10-10 09:30".to_owned(),
        fare_difference_cents: 2_500,
    }
}

/// A command the airline's own systems send, outside any turn, under a key naming the
/// delivery: the same batch delivered twice is one effect.
fn outside(
    trips: &InMemoryExecutor<TripWorkflow>,
    account: &AccountId,
    key: &str,
    command: TripCommand,
) -> CommandBatch<TripCommand> {
    let seed = key.bytes().fold(0xa1_u128, |hash, byte| {
        hash.wrapping_mul(131).wrapping_add(u128::from(byte))
    });
    let turn_id = TurnId::from(uuid::Uuid::from_u128(seed));
    let case_ref = CaseRef::new(TRIP, CASE, trips.revision_of(account, &CaseId::from(CASE)));
    CommandBatch {
        batch_id: BatchId::derive(&turn_id, &case_ref.key(), &AtomicityScope::PerCase),
        scope: AtomicityScope::PerCase,
        envelopes: vec![CommandEnvelope {
            command_id: CommandId::derive(&turn_id, ActId::new(UnitId(1), 1), 0),
            turn_id,
            actor: ActorContext::new(account.clone(), "airline"),
            case_ref,
            idempotency_key: IdempotencyKey::new(format!("airline:{key}")),
            origin: CommandOrigin::ExternalCallback {
                callback_id: key.to_owned(),
                signature_verified: true,
            },
            command,
        }],
    }
}

/// The airline behind the outbox, which never answers a send.
struct SilentAirline;

#[async_trait]
impl OutboxSender for SilentAirline {
    async fn send(&self, _entry: &OutboxEntry) -> Dispatched {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Dispatched::completed()
    }
}

/// Asked later, the airline says it has the rebooking.
struct AirlineHasIt;

#[async_trait]
impl OutboxReconciler for AirlineHasIt {
    async fn reconcile(&self, _record: &OutboxRecord) -> Reconciled {
        Reconciled::Completed
    }
}

/// The conservative configuration with narration off, so every printed line is server-owned.
fn quiet_config() -> OrchestratorConfig {
    let mut config = OrchestratorConfig::conservative();
    config.narration = NarrationConfig::conservative().with_enabled(false);
    config
}

/// The opaque token the runtime issues for the trip in this turn.
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

/// A click on `card`'s confirm option, at the revision the card was shown for.
fn click(
    account: &AccountId,
    conversation: ConversationId,
    turn_id: TurnId,
    card: &Interaction,
) -> TurnInput {
    TurnInput {
        text: None,
        interaction_response: Some(InteractionResponse {
            interaction_id: card.id,
            option_id: REBOOK_CONFIRM_OPTION.into(),
            expected_case_revision: card.case_ref.expected_revision,
            freeform_input: None,
        }),
        ..text_turn(account, conversation, turn_id, "")
    }
}

async fn load(trips: &Arc<InMemoryExecutor<TripWorkflow>>, account: &AccountId) -> TripState {
    WorkflowExecutor::load(trips.as_ref(), account, &CaseId::from(CASE))
        .await
        .expect("the in-memory executor always answers")
        .value
        .expect("the trip exists")
}

async fn events(stores: &FakeStores, account: &AccountId, case: &CaseKey) -> Vec<String> {
    stores
        .event_types(account, case)
        .await
        .expect("the store answers")
}

async fn blocking_card(stores: &FakeStores, account: &AccountId, case: &CaseKey) -> Interaction {
    stores
        .open_interactions(account, case)
        .await
        .expect("the store answers")
        .into_iter()
        .find(|card| card.blocking)
        .expect("the trip has a blocking card")
}

fn notice_codes(turn: &AssistantTurn) -> Vec<String> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Notice(notice) => Some(notice.code.clone()),
            _ => None,
        })
        .collect()
}

fn print_legs(trip: &TripState) {
    for leg in &trip.legs {
        println!(
            "  leg {}  {} {}→{} {}  {:?}{}",
            leg.number,
            leg.flight,
            leg.from,
            leg.to,
            leg.departs,
            leg.status,
            if leg.protected { "  kept as it is" } else { "" }
        );
    }
    if let Some(offer) = &trip.offer {
        println!(
            "  offer  leg {} on {} {}, {:.2} EUR more",
            offer.leg,
            offer.flight,
            offer.departs,
            offer.fare_difference_cents as f64 / 100.0
        );
    }
    println!();
}

fn print_card(card: &Interaction) {
    let locale = Locale::from("en-GB");
    println!("  card      {:?}", card.payload.title.resolve(&locale));
    if let Some(body) = &card.payload.body {
        println!("  body      {:?}", body.resolve(&locale));
    }
    println!(
        "  bound to  {} @ revision {}",
        card.case_ref.case_id, card.case_ref.expected_revision
    );
    for option in &card.payload.options {
        println!(
            "  option    {:<8} {:?}",
            option.id.as_str(),
            option.label.resolve(&locale)
        );
    }
    println!();
}

/// The blocks of a turn, in the order they were returned and persisted.
fn print_turn(turn: &AssistantTurn) {
    let locale = Locale::from("en-GB");
    if turn.blocks.is_empty() {
        println!("  reply     (no blocks)\n");
        return;
    }
    for block in &turn.blocks {
        match block {
            ResponseBlock::Receipt(receipt) => println!(
                "  receipt   {:<30} {:?}",
                receipt.receipt.status_code,
                receipt.receipt.body.resolve(&locale)
            ),
            ResponseBlock::Notice(notice) => println!(
                "  notice    {:<30} {:?}",
                notice.code,
                notice.text.resolve(&locale)
            ),
            ResponseBlock::Interaction(card) => println!(
                "  card      {:<30} {:?}",
                format!("{:?}", card.view.kind),
                card.view.title.resolve(&locale)
            ),
            ResponseBlock::Transition(transition) => {
                println!("  asks      {:?}", transition.text);
            }
            ResponseBlock::Artifact(document) => println!(
                "  document  {:<30} {:?}",
                document.artifact.kind,
                document.artifact.label.resolve(&locale)
            ),
            other => println!("  block     {other:?}"),
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
        "-".repeat(70_usize.saturating_sub(title.chars().count()))
    );
}
