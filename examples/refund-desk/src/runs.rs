//! The ten runs: one without an attack, nine with one. Each seeds its own desk, scripts what
//! the model reads, and records what the runtime does with it. What a run authors is its
//! attack line and the lines saying what happened outside the turn; the rest is recorded.

use serde_json::json;
use turnframe::error::{OrchestratorError, StoreError};
use turnframe::event::ExternalStatus;
use turnframe::interaction::Interaction;
use turnframe::interaction::InteractionRejection;
use turnframe::plan::AnswerBasis;
use turnframe::provider::error::ProviderError;
use turnframe::response::AssistantTurn;
use turnframe::tasks::testing::ScriptedTasks;
use turnframe::testing::providers::UnderstandingBuilder;
use turnframe::turn::TurnInput;
use turnframe::understanding::{ActTarget, ConstraintKind, Understanding};

use crate::desk::{Desk, Provider, token, turn_id};
use crate::order::{KEEP_OPTION, OrderCommand, REFUND_OPTION, operations};
use crate::record::{OrderLine, Recorder, Recording, Run, Station};

/// The message of every run but two.
pub const REFUND: &str = "Refund order 381 for €129";
const TAKE_BACK: &str =
    "Refund order 381 for €129. Actually don't refund it yet, just tell me whether it's eligible.";
const UNSEEN: &str = "Refund order 402 for €129";
const ELIGIBLE: &str = "Yes: order 381 was delivered on 18 September, and its refund window is \
                        open until 18 October.";

/// The refund of `cents` on order `number`, as the model reads it in turn `turn`.
fn refund_of(turn: u128, number: u32, cents: i64, text: &str) -> Understanding {
    UnderstandingBuilder::of(text)
        .apply(
            operations::REQUEST_REFUND,
            token(turn, number),
            json!({ "amount": { "minor": cents, "currency": "EUR" } }),
            REFUND,
        )
        .build()
        .expect("the words are in the message")
}

/// Names the record a target of turn `turn` stands for.
fn labels(turn: u128) -> impl Fn(&ActTarget) -> String {
    move |target| match target {
        ActTarget::Record { token: aimed } => [381, 318]
            .into_iter()
            .find(|number| token(turn, *number) == *aimed)
            .map_or_else(|| "a record".to_owned(), |number| format!("Order {number}")),
        ActTarget::NotListed { .. } => "an order not in view".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Handles `input` and records the turn; a turn the runtime refuses is recorded as refused.
async fn handle(
    desk: &Desk,
    recorder: &mut Recorder,
    input: TurnInput,
) -> anyhow::Result<Option<AssistantTurn>> {
    let id = input.turn_id;
    match desk.orchestrator.handle_turn(input).await {
        Ok(answered) => {
            let record = desk.stores.replay_record(&desk.account, &id).await?;
            recorder.turn(&record, &answered, &desk.orders.entries());
            Ok(Some(answered))
        }
        Err(refused) => {
            recorder.refused(Station::Decision, &refusal(&refused));
            Ok(None)
        }
    }
}

/// A turn the runtime refused whole, in words.
fn refusal(error: &OrchestratorError) -> String {
    match error {
        OrchestratorError::Store(StoreError::Conflict) => {
            "Refused whole: a turn with this id is already on record.".to_owned()
        }
        OrchestratorError::Interaction(turnframe::error::InteractionError::Rejected(
            InteractionRejection::Stale {
                bound_revision,
                current_revision,
                ..
            },
        )) => format!(
            "Refused whole: the card was drawn at revision {bound_revision}, and the order is \
             at revision {current_revision}. The click authorizes nothing."
        ),
        other => format!("Refused whole: {other}."),
    }
}

/// The open card on order `number`, which the run cannot go on without.
async fn card(desk: &Desk, number: u32) -> anyhow::Result<Interaction> {
    desk.card(number)
        .await
        .ok_or_else(|| anyhow::anyhow!("order {number} has no open card"))
}

/// Clicks `option` of `card` as turn `turn`.
async fn click(
    desk: &Desk,
    recorder: &mut Recorder,
    turn: u128,
    card: &Interaction,
    option: &str,
) -> anyhow::Result<()> {
    recorder.click(&card.view(), option);
    handle(desk, recorder, desk.click(turn, card, option)).await?;
    Ok(())
}

/// The provider's answer, from outside any turn, delivered `times` times.
async fn provider_answers(
    desk: &Desk,
    recorder: &mut Recorder,
    times: usize,
) -> anyhow::Result<()> {
    let replays = desk
        .deliver(
            381,
            "provider-answer",
            OrderCommand::RecordProviderOutcome {
                status: ExternalStatus::Accepted,
                reference: Some("re_381".to_owned()),
            },
            times,
        )
        .await?;
    recorder.ledger(&desk.orders.entries());
    if replays.iter().skip(1).all(|replay| *replay) && times > 1 {
        recorder.world(
            Station::Ledger,
            "The second delivery is the same batch under the same key: a replay, recorded once.",
        );
    }
    Ok(())
}

async fn outbox(desk: &Desk, recorder: &mut Recorder, turn: u128) -> anyhow::Result<()> {
    for row in desk.outbox(turn).await? {
        recorder.outbox(&row);
    }
    Ok(())
}

struct Meta {
    id: &'static str,
    group: &'static str,
    label: &'static str,
    attack: &'static str,
}

async fn finish(meta: Meta, message: &str, recorder: Recorder, desk: &Desk) -> Run {
    let open = desk.card(381).await.is_some() || desk.card(318).await.is_some();
    let (frames, stopped_at, verdict) = recorder.finish(&desk.orders.entries(), open);
    Run {
        id: meta.id,
        group: meta.group,
        label: meta.label,
        attack: meta.attack,
        message: message.to_owned(),
        stopped_at,
        verdict,
        frames,
    }
}

/// The first turn of a run: the message, read as `understood`.
async fn asked(
    desk: &Desk,
    recorder: &mut Recorder,
    message: &str,
    understood: &Understanding,
) -> anyhow::Result<()> {
    recorder.message(message);
    recorder.reading(message, understood, &labels(1));
    handle(desk, recorder, desk.send(1, message)).await?;
    Ok(())
}

/// No attack: the card, the click, the send, the answer.
///
/// # Errors
///
/// When the runtime fails.
pub async fn no_attack() -> anyhow::Result<(Run, Desk)> {
    let understood = refund_of(1, 381, 12_900, REFUND);
    let desk = Desk::scripted(vec![understood.clone()], &[]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, REFUND, &understood).await?;
    let shown = card(&desk, 381).await?;
    click(&desk, &mut recorder, 2, &shown, REFUND_OPTION).await?;
    desk.dispatch(Provider::Accepts).await?;
    outbox(&desk, &mut recorder, 2).await?;
    recorder.world(
        Station::Ledger,
        "The payment provider answers: accepted, reference re_381.",
    );
    provider_answers(&desk, &mut recorder, 1).await?;
    let meta = Meta {
        id: "no-attack",
        group: "none",
        label: "No attack",
        attack: "Nothing goes wrong: the refund is asked for, confirmed and sent.",
    };
    Ok((finish(meta, REFUND, recorder, &desk).await, desk))
}

/// The model reads order 318; the card names it, and the person declines.
///
/// # Errors
///
/// When the runtime fails.
pub async fn wrong_order() -> anyhow::Result<(Run, Desk)> {
    let understood = refund_of(1, 318, 12_900, REFUND);
    let desk = Desk::scripted(vec![understood.clone()], &[]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, REFUND, &understood).await?;
    let shown = card(&desk, 318).await?;
    recorder.stop(Station::Decision);
    click(&desk, &mut recorder, 2, &shown, KEEP_OPTION).await?;
    let meta = Meta {
        id: "wrong-order",
        group: "model",
        label: "Picks the wrong order",
        attack: "The model reads order 318, another customer's, for order 381.",
    };
    Ok((finish(meta, REFUND, recorder, &desk).await, desk))
}

/// The model reads €1,290; the domain refuses more than was paid.
///
/// # Errors
///
/// When the runtime fails.
pub async fn wrong_amount() -> anyhow::Result<(Run, Desk)> {
    let understood = refund_of(1, 381, 129_000, REFUND);
    let desk = Desk::scripted(vec![understood.clone()], &[]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, REFUND, &understood).await?;
    recorder.stop(Station::Reducer);
    let meta = Meta {
        id: "wrong-amount",
        group: "model",
        label: "Reads the wrong amount",
        attack: "The model reads €1,290 for €129.",
    };
    Ok((finish(meta, REFUND, recorder, &desk).await, desk))
}

/// The message names another shop's order; the desk's directory has none such.
///
/// # Errors
///
/// When the runtime fails.
pub async fn unseen_order() -> anyhow::Result<(Run, Desk)> {
    let understood = UnderstandingBuilder::of(UNSEEN)
        .apply_to_unlisted(
            operations::REQUEST_REFUND,
            "order",
            "order 402",
            json!({ "amount": { "minor": 12_900, "currency": "EUR" } }),
            UNSEEN,
        )
        .build()
        .expect("the words are in the message");
    let desk = Desk::scripted(vec![understood.clone()], &[]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, UNSEEN, &understood).await?;
    recorder.stop(Station::Reducer);
    let meta = Meta {
        id: "unseen-order",
        group: "model",
        label: "Names an order this desk can't see",
        attack: "Order 402 belongs to another shop. The model reads it as written, and it has \
                 no id to give: only the desk's directory can find a record.",
    };
    Ok((finish(meta, UNSEEN, recorder, &desk).await, desk))
}

/// The refund is taken back in the same message, and eligibility asked.
///
/// # Errors
///
/// When the runtime fails.
pub async fn take_back() -> anyhow::Result<(Run, Desk)> {
    let understood = UnderstandingBuilder::of(TAKE_BACK)
        .apply(
            operations::REQUEST_REFUND,
            token(1, 381),
            json!({ "amount": { "minor": 12_900, "currency": "EUR" } }),
            "Refund order 381 for €129.",
        )
        .superseded_by_next()
        .constrain(ConstraintKind::DoNotSubmit, "Actually don't refund it yet,")
        .ask_about(
            AnswerBasis::CurrentCommittedState,
            Some(token(1, 381)),
            &["refund_window"],
            "just tell me whether it's eligible.",
        )
        .build()
        .expect("the words are in the message");
    let desk = Desk::scripted(vec![understood.clone()], &[ELIGIBLE]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, TAKE_BACK, &understood).await?;
    recorder.stop(Station::Reading);
    let meta = Meta {
        id: "take-back",
        group: "user",
        label: "Takes it back mid-message",
        attack: "The message asks for the refund, then takes it back and asks a question.",
    };
    Ok((finish(meta, TAKE_BACK, recorder, &desk).await, desk))
}

/// Refund clicked twice.
///
/// # Errors
///
/// When the runtime fails.
pub async fn double_click() -> anyhow::Result<(Run, Desk)> {
    let understood = refund_of(1, 381, 12_900, REFUND);
    let desk = Desk::scripted(vec![understood.clone()], &[]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, REFUND, &understood).await?;
    let shown = card(&desk, 381).await?;
    click(&desk, &mut recorder, 2, &shown, REFUND_OPTION).await?;
    click(&desk, &mut recorder, 3, &shown, REFUND_OPTION).await?;
    recorder.stop(Station::Decision);
    desk.dispatch(Provider::Accepts).await?;
    outbox(&desk, &mut recorder, 2).await?;
    outbox(&desk, &mut recorder, 3).await?;
    recorder.world(
        Station::Ledger,
        "The payment provider answers: accepted, reference re_381.",
    );
    provider_answers(&desk, &mut recorder, 1).await?;
    let meta = Meta {
        id: "double-click",
        group: "user",
        label: "Double-clicks Refund",
        attack: "The Refund button is clicked twice.",
    };
    Ok((finish(meta, REFUND, recorder, &desk).await, desk))
}

/// The app sends the same turn twice.
///
/// # Errors
///
/// When the runtime fails.
pub async fn sent_twice() -> anyhow::Result<(Run, Desk)> {
    let understood = refund_of(1, 381, 12_900, REFUND);
    let desk = Desk::scripted(vec![understood.clone(), understood.clone()], &[]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, REFUND, &understood).await?;
    recorder.world(
        Station::Decision,
        "The app did not hear back, and sends the same turn again.",
    );
    handle(&desk, &mut recorder, desk.send(1, REFUND)).await?;
    recorder.stop(Station::Decision);
    let shown = card(&desk, 381).await?;
    click(&desk, &mut recorder, 2, &shown, REFUND_OPTION).await?;
    desk.dispatch(Provider::Accepts).await?;
    outbox(&desk, &mut recorder, 2).await?;
    recorder.world(
        Station::Ledger,
        "The payment provider answers: accepted, reference re_381.",
    );
    provider_answers(&desk, &mut recorder, 1).await?;
    let meta = Meta {
        id: "sent-twice",
        group: "user",
        label: "The request arrives twice",
        attack: "The app resends the same turn after a dropped connection.",
    };
    Ok((finish(meta, REFUND, recorder, &desk).await, desk))
}

/// A colleague refunds part of the order while the card is open.
///
/// # Errors
///
/// When the runtime fails.
pub async fn stale_card() -> anyhow::Result<(Run, Desk)> {
    let understood = refund_of(1, 381, 12_900, REFUND);
    let again = UnderstandingBuilder::of("where does it stand?")
        .build()
        .expect("nothing to point at");
    let desk = Desk::scripted(vec![understood.clone(), again], &[]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, REFUND, &understood).await?;
    let shown = card(&desk, 381).await?;
    recorder.world(
        Station::Ledger,
        "While the card is open, a colleague refunds €30.00 of the order.",
    );
    desk.outside(
        381,
        "colleague",
        OrderCommand::RefundOutside {
            cents: 3_000,
            by: "a colleague".to_owned(),
        },
    )
    .await?;
    recorder.ledger(&desk.orders.entries());
    click(&desk, &mut recorder, 2, &shown, REFUND_OPTION).await?;
    recorder.stop(Station::Decision);
    if desk.card(381).await.is_some_and(|open| open.id == shown.id) {
        recorder.message("where does it stand?");
        handle(&desk, &mut recorder, desk.send(3, "where does it stand?")).await?;
    }
    let meta = Meta {
        id: "stale-card",
        group: "world",
        label: "The order changes under the card",
        attack: "A colleague refunds €30.00 of the order while the card is open.",
    };
    Ok((finish(meta, REFUND, recorder, &desk).await, desk))
}

/// The payment provider takes the refund and never answers.
///
/// # Errors
///
/// When the runtime fails.
pub async fn timeout() -> anyhow::Result<(Run, Desk)> {
    let understood = refund_of(1, 381, 12_900, REFUND);
    let desk = Desk::scripted(vec![understood.clone()], &[]).await?;
    let mut recorder = Recorder::default();
    asked(&desk, &mut recorder, REFUND, &understood).await?;
    let shown = card(&desk, 381).await?;
    click(&desk, &mut recorder, 2, &shown, REFUND_OPTION).await?;
    recorder.world(
        Station::Ledger,
        "The payment provider takes the refund, and never answers.",
    );
    desk.dispatch(Provider::Silent).await?;
    outbox(&desk, &mut recorder, 2).await?;
    recorder.stop(Station::Ledger);
    recorder.world(Station::Ledger, "The outbox runs again.");
    desk.dispatch(Provider::Silent).await?;
    outbox(&desk, &mut recorder, 2).await?;
    for row in desk.outbox(2).await? {
        let settled = desk.reconcile(&row).await?;
        recorder.world(
            Station::Ledger,
            &format!("Asked later, the provider says it has the refund: {settled:?}."),
        );
    }
    recorder.world(Station::Ledger, "Its answer arrives, twice.");
    provider_answers(&desk, &mut recorder, 2).await?;
    let meta = Meta {
        id: "timeout",
        group: "world",
        label: "The payment provider goes silent",
        attack: "The payment provider takes the refund and never answers.",
    };
    Ok((finish(meta, REFUND, recorder, &desk).await, desk))
}

/// The model provider fails halfway through the reading.
///
/// # Errors
///
/// When the runtime fails.
pub async fn model_down() -> anyhow::Result<(Run, Desk)> {
    let segment = json!({ "analysis": "One request: a refund.", "units": [
        { "kind": "request", "words": { "from": 1, "to": 5 }, "workflow": "order" }
    ]});
    let routed = json!({ "operations": [operations::REQUEST_REFUND] });
    let tasks = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment#vote1", segment.clone())
        .answer("turn/segment#vote2", segment.clone())
        .answer("turn/segment#vote3", segment)
        .answer("turn/coverage", json!({ "missed": [] }))
        .answer("u1/route#vote1", routed.clone())
        .answer("u1/route#vote2", routed.clone())
        .answer("u1/route#vote3", routed)
        .answer("u1/locate", json!({ "record": "r1", "named": null }));
    // Every attempt times out: the first call and each retry the engine makes.
    let tasks = (0..6).fold(tasks, |tasks, _| {
        tasks.failing("u1/extract", ProviderError::timeout())
    });
    let desk = Desk::reading(tasks).await?;
    let mut recorder = Recorder::default();
    recorder.message(REFUND);
    recorder.world(
        Station::Reading,
        "The model answers the first questions, then stops answering: every call times out.",
    );
    let at = recorder.frames.len();
    let answered = handle(&desk, &mut recorder, desk.send(1, REFUND)).await?;
    if answered.is_some() {
        let record = desk
            .stores
            .replay_record(&desk.account, &turn_id(1))
            .await?;
        if let Some(understanding) = record.understanding {
            // What the pipeline read comes before what the runtime did with it.
            let mut reading = Recorder::default();
            reading.reading(REFUND, &understanding, &labels(1));
            recorder.frames.splice(at..at, reading.frames);
        }
    }
    recorder.stop(Station::Reading);
    let meta = Meta {
        id: "model-down",
        group: "world",
        label: "The model provider fails",
        attack: "The model provider stops answering halfway through the reading.",
    };
    Ok((finish(meta, REFUND, recorder, &desk).await, desk))
}

/// Every run, in the page's order.
///
/// # Errors
///
/// When a run fails.
pub async fn all() -> anyhow::Result<Recording> {
    // One run at a time: each desk is dropped before the next is built.
    let mut runs = Vec::new();
    runs.push(Box::pin(no_attack()).await?.0);
    runs.push(Box::pin(wrong_order()).await?.0);
    runs.push(Box::pin(wrong_amount()).await?.0);
    runs.push(Box::pin(unseen_order()).await?.0);
    runs.push(Box::pin(take_back()).await?.0);
    runs.push(Box::pin(double_click()).await?.0);
    runs.push(Box::pin(sent_twice()).await?.0);
    runs.push(Box::pin(stale_card()).await?.0);
    runs.push(Box::pin(timeout()).await?.0);
    runs.push(Box::pin(model_down()).await?.0);
    let order = |label: &str, customer: &str, paid: &str, delivered: &str, listed| OrderLine {
        label: label.to_owned(),
        customer: customer.to_owned(),
        paid: paid.to_owned(),
        delivered: delivered.to_owned(),
        listed,
    };
    Ok(Recording {
        recorded_with: "cargo run -p refund-desk -- --record website/src/data/refund-runs.json",
        version: env!("CARGO_PKG_VERSION"),
        orders: vec![
            order("Order 381", "Giulia Neri", "€129.00", "18 September", true),
            order("Order 318", "Luca Moretti", "€189.00", "10 September", true),
            order(
                "Order 402",
                "another shop's customer",
                "€129.00",
                "25 September",
                false,
            ),
        ],
        runs,
    })
}

/// The recording as the site reads it.
///
/// # Errors
///
/// When it cannot be serialized.
pub fn json(recording: &Recording) -> anyhow::Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(recording)?))
}
