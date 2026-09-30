//! One conversation held with the runtime, turn by turn, until the goal or the limit.

use futures::StreamExt as _;
use turnframe_core::ids::TurnId;
use turnframe_core::interaction::Interaction;
use turnframe_core::locale::Locale;
use turnframe_core::response::{AssistantTurn, Expectation, ResponseBlock};
use turnframe_core::turn::{InteractionResponse, TurnInput};
use turnframe_runtime::orchestrator::error_code;
use turnframe_store::interaction::InteractionReader;

use super::goal::Goal;
use super::score::{Conversation, Ending, Exchange, SimulationReport, score};
use super::user::{CardOnScreen, Screen, SimulatedUser, UserMove};
use crate::assertions::check;
use crate::observation::Observation;
use crate::runner::{EvalHarness, PreparedRun, SampleIndex};

/// The id of turn `number` of a conversation whose first turn is `first`.
#[must_use]
pub fn turn_at(first: TurnId, number: u32) -> TurnId {
    let offset = u128::from(number) << 64;
    TurnId::from(uuid::Uuid::from_u128(
        first.as_uuid().as_u128().wrapping_add(offset),
    ))
}

/// Runs goals against a harness, each manner of each goal `samples` times.
#[derive(Debug, Clone, Copy)]
pub struct Simulation {
    samples: u32,
    concurrency: usize,
}

impl Default for Simulation {
    fn default() -> Self {
        Self {
            samples: 1,
            concurrency: 1,
        }
    }
}

impl Simulation {
    /// One sample of each, one at a time.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Holds each conversation `samples` times.
    #[must_use]
    pub const fn with_samples(mut self, samples: u32) -> Self {
        self.samples = samples;
        self
    }

    /// Holds at most `concurrency` conversations at once; the report's order is kept.
    #[must_use]
    pub const fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency;
        self
    }

    /// Holds every conversation and reports them.
    pub async fn run(
        &self,
        goals: &[Goal],
        harness: &dyn EvalHarness,
        user: &dyn SimulatedUser,
    ) -> SimulationReport {
        let runs = goals.iter().flat_map(|goal| {
            goal.manners.iter().flat_map(move |manner| {
                (0..self.samples.max(1)).map(move |sample| (goal, manner, SampleIndex(sample)))
            })
        });
        let conversations = futures::stream::iter(
            runs.map(|(goal, manner, sample)| converse(goal, manner, harness, user, sample)),
        )
        .buffered(self.concurrency.max(1))
        .collect()
        .await;
        SimulationReport { conversations }
    }
}

/// Holds one conversation: `user` pursues `goal` in `manner` until done or the limit.
pub async fn converse(
    goal: &Goal,
    manner: &str,
    harness: &dyn EvalHarness,
    user: &dyn SimulatedUser,
    sample: SampleIndex,
) -> Conversation {
    let mut conversation = Conversation {
        goal: goal.id.clone(),
        manner: manner.to_owned(),
        sample: sample.number(),
        exchanges: Vec::new(),
        ended: Ending::TurnLimit,
        score: score(&[], false),
    };
    let prepared = match harness.prepare(&goal.item(), sample).await {
        Ok(prepared) => prepared,
        Err(error) => {
            conversation.ended = Ending::Unprepared {
                message: error.to_string(),
            };
            return conversation;
        }
    };
    let locale = goal.locale.clone().unwrap_or_else(|| Locale::from("en"));
    let mut read: Vec<(UserMove, String)> = Vec::new();
    let mut ran: Vec<TurnId> = Vec::new();
    let mut offers: Vec<String> = Vec::new();
    for number in 0..goal.max_turns {
        let open = open_cards(&prepared).await;
        let cards: Vec<CardOnScreen> = open.iter().map(|card| on_screen(card, &locale)).collect();
        let screen = Screen {
            want: &goal.want,
            manner,
            exchanges: &read,
            cards: &cards,
            offers: &offers,
            turns_left: goal.max_turns - number,
        };
        let said = match user.next(&screen).await {
            Ok(UserMove::Done) => {
                conversation.ended = Ending::Done;
                break;
            }
            Ok(said) => said,
            Err(message) => {
                conversation.ended = Ending::UserFailed { message };
                break;
            }
        };
        let (text, response) = match &said {
            UserMove::Say { text } => (Some(text.clone()), None),
            UserMove::Press { option } => match pressed(&open, option, &locale) {
                Some(response) => (None, Some(response)),
                // No button is called that: nothing is sent, and the person sees so.
                None => {
                    let note = format!("(no card on screen has an option «{option}»)");
                    read.push((said.clone(), note));
                    continue;
                }
            },
            UserMove::Done => (None, None),
        };
        let turn_id = turn_at(prepared.turn_id, number);
        let outcome = prepared
            .orchestrator
            .handle_turn(input(&prepared, text, response, turn_id, &locale))
            .await;
        let observed = Observation::collect(
            prepared.orchestrator.stores(),
            prepared.workflows.as_ref(),
            &prepared.actor.account_id,
            turn_id,
            &goal.setup.cases,
            outcome.as_ref().map_err(error_code),
        )
        .await;
        ran.push(turn_id);
        let blocking_open = open_cards(&prepared).await.iter().any(|card| card.blocking);
        offers = outcome.as_ref().map_or_else(
            |_| Vec::new(),
            |turn| {
                turn.offers
                    .iter()
                    .map(|offer| offer.words.clone())
                    .collect()
            },
        );
        let exchange = match &outcome {
            Ok(turn) => answered(turn, &observed, &locale, blocking_open),
            Err(error) => Exchange {
                failed: Some(error_code(error)),
                ..Exchange::default()
            },
        };
        read.push((said.clone(), exchange.reply.clone()));
        conversation.exchanges.push(Exchange {
            said: Some(said),
            ..exchange
        });
    }
    let reached = reached(goal, &prepared, &ran).await;
    conversation.score = score(&conversation.exchanges, reached);
    conversation
}

/// Whether the goal's state holds now, read from the stores.
async fn reached(goal: &Goal, prepared: &PreparedRun, ran: &[TurnId]) -> bool {
    let (last, earlier) = match ran.split_last() {
        Some((last, earlier)) => (*last, earlier),
        None => (prepared.turn_id, &[][..]),
    };
    let mut observed = Observation::collect(
        prepared.orchestrator.stores(),
        prepared.workflows.as_ref(),
        &prepared.actor.account_id,
        last,
        &goal.setup.cases,
        Err(String::new()),
    )
    .await
    .with_conversation_cases(
        prepared.orchestrator.stores(),
        prepared.workflows.as_ref(),
        &prepared.actor.account_id,
        earlier,
    )
    .await;
    // Only the state is asked about, whatever became of the last turn.
    observed.error_code = None;
    check(&goal.reached.expectations(), &observed).is_empty()
}

/// The cards open in the conversation, blocking ones first.
async fn open_cards(prepared: &PreparedRun) -> Vec<Interaction> {
    let store = prepared.orchestrator.stores().interactions();
    let mut open = InteractionReader::list_open_for_conversation(
        store.as_ref(),
        &prepared.actor.account_id,
        &prepared.conversation_id,
    )
    .await
    .unwrap_or_default();
    open.sort_by_key(|card| !card.blocking);
    open
}

fn on_screen(card: &Interaction, locale: &Locale) -> CardOnScreen {
    CardOnScreen {
        title: card.payload.title.resolve(locale).to_owned(),
        options: card
            .payload
            .options
            .iter()
            .map(|option| {
                (
                    option.id.as_str().to_owned(),
                    option.label.resolve(locale).to_owned(),
                )
            })
            .collect(),
    }
}

/// The answer to the card option a press names, by its id or its label.
fn pressed(open: &[Interaction], option: &str, locale: &Locale) -> Option<InteractionResponse> {
    open.iter().find_map(|card| {
        let chosen = card.payload.options.iter().find(|offered| {
            offered.id.as_str().eq_ignore_ascii_case(option)
                || offered.label.resolve(locale).eq_ignore_ascii_case(option)
        })?;
        Some(InteractionResponse {
            interaction_id: card.id,
            option_id: chosen.id.clone(),
            expected_case_revision: card.case_ref.expected_revision,
            freeform_input: None,
        })
    })
}

fn input(
    prepared: &PreparedRun,
    text: Option<String>,
    interaction_response: Option<InteractionResponse>,
    turn_id: TurnId,
    locale: &Locale,
) -> TurnInput {
    TurnInput {
        turn_id,
        conversation_id: prepared.conversation_id,
        actor: prepared.actor.clone(),
        text,
        interaction_response,
        attachments: Vec::new(),
        origin: None,
        locale: locale.clone(),
        effort: None,
    }
}

/// What a turn that answered showed, and what code reads of it.
fn answered(
    turn: &AssistantTurn,
    observed: &Observation,
    locale: &Locale,
    blocking_open: bool,
) -> Exchange {
    // The reply carries its answers: their blocks are read only when there is no reply.
    let replied = turn
        .blocks
        .iter()
        .any(|block| matches!(block, ResponseBlock::Transition(_)));
    let mut shown = Vec::new();
    let mut prose = String::new();
    let mut card = false;
    for block in &turn.blocks {
        match block {
            ResponseBlock::Transition(transition) => {
                prose.clone_from(&transition.text);
                shown.push(transition.text.clone());
            }
            ResponseBlock::Answer(answer) if !replied => shown.push(answer.text.clone()),
            ResponseBlock::Receipt(receipt) => shown.push(format!(
                "[{}: {}]",
                receipt.receipt.title.resolve(locale),
                receipt.receipt.body.resolve(locale)
            )),
            ResponseBlock::Notice(notice) => {
                shown.push(format!("[{}]", notice.text.resolve(locale)));
            }
            ResponseBlock::Interaction(_) => card = true,
            _ => {}
        }
    }
    let asks_a_question = prose
        .trim_end()
        .trim_end_matches(['"', '»', ')', '*', '_'])
        .ends_with('?');
    // An ask the turn recorded is a way forward however it is worded: «Still needed: X.».
    let asks: Vec<String> = turn.expectations.iter().filter_map(asked).collect();
    Exchange {
        said: None,
        reply: shown.join("\n"),
        failed: None,
        way_forward: asks_a_question
            || !asks.is_empty()
            || card
            || blocking_open
            || !turn.offers.is_empty(),
        asks,
        offers: turn
            .offers
            .iter()
            .map(|offer| offer.operation.as_str().to_owned())
            .collect(),
        refused: observed
            .acts
            .iter()
            .filter(|act| act.outcome.as_deref() == Some("rejected"))
            .filter_map(|act| act.operation.clone())
            .collect(),
        not_understood: observed
            .understanding
            .as_ref()
            .map_or(0, |understanding| understanding.not_understood.len()),
    }
}

/// What an expectation asked, as its record and what it waits on.
fn asked(expectation: &Expectation) -> Option<String> {
    Some(match expectation {
        Expectation::AwaitingObligation {
            case_ref,
            obligation,
        }
        | Expectation::AwaitingOperation {
            case_ref,
            obligation,
            ..
        } => format!("{}/{}: {obligation}", case_ref.workflow, case_ref.case_id),
        Expectation::AwaitingValue {
            act,
            case_ref,
            missing,
        } => format!(
            "{}: {} {}",
            case_ref.as_ref().map_or_else(String::new, |case| format!(
                "{}/{}",
                case.workflow, case.case_id
            )),
            act.operation().map_or("", |operation| operation.as_str()),
            missing.join(",")
        ),
        _ => return None,
    })
}
