//! The trip [`WorkflowDefinition`]: projection, catalog, compilation, policy,
//! validation and receipts.
//!
//! [`PhaseOwnership::User`] means "the user must answer a blocking card", not "it
//! is the user's move". Free-form collection is therefore [`PhaseOwnership::System`];
//! only `AwaitingRebookingConfirmation` is user-owned, because only there does a
//! persistent card own what a bare "yes" means.

use serde::de::DeserializeOwned;
use turnframe_core::case::CaseRef;
use turnframe_core::command::{
    AtomicityScope, ClaimMode, CommandPolicy, ConfirmationPolicy, RiskClass,
};
use turnframe_core::error::DomainRejection;
use turnframe_core::event::{
    ArtifactRef, CommittedEvent, OperationalReceipt, ReceiptEvent, ReceiptSeverity, RedactedEvent,
};
use turnframe_core::flow::{
    ConfirmationSubject, DomainEnumeration, EnumeratedValue, InteractionRequirement,
    PhaseOwnership, StateField, ViewOf, WorkflowDefinition, WorkflowNotice, WorkflowView,
};
use turnframe_core::hash::canonical_digest;
use turnframe_core::ids::{OperationKey, ReceiptId, WorkflowKey, WorkflowVersion};
use turnframe_core::interaction::{
    InteractionKind, InteractionOption, InteractionPayload, InteractionSpec, OptionStyle,
    StoredInteractionAction,
};
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::operation::{DateDirection, GlossaryTerm, OperationSpec};
use turnframe_core::plan::TargetPolicy;
use turnframe_core::response::NoticeSeverity;
use turnframe_core::target::{ResolvedAct, ResolvedActKind};

use crate::workflows::trip::apply::{apply, reject, rejection, validate};
use crate::workflows::trip::command::{
    AddExtraArgs, AssignPayerArgs, ChangeExtraArgs, ChangeTravelerArgs, LegArgs, OpenArgs,
    SetNameArgs, SetTravelDateArgs, SetTravelerArgs, TravelerRecord, TripCommand, TripEvent,
    operations,
};
use crate::workflows::trip::state::{
    NewExtra, Payer, TripObligation, TripOutcome, TripPhase, TripState, TripStatus, TripTraveler,
};
use crate::workflows::{Applied, PureWorkflow};

/// Key of the blocking rebooking confirmation requirement.
pub const REBOOKING_CONFIRMATION_KEY: &str = "trip.rebooking_confirmation";

/// Option that confirms the rebooking.
pub const REBOOK_CONFIRM_OPTION: &str = "confirm";

/// Option that keeps the booked flight.
pub const REBOOK_DECLINE_OPTION: &str = "decline";

/// The trip workflow: the disruption case of one booking.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct TripWorkflow {
    with_cards: bool,
}

impl TripWorkflow {
    /// Builds the workflow: a rebooking is the only step a card confirms.
    #[must_use]
    pub const fn new() -> Self {
        Self { with_cards: false }
    }

    /// The same workflow with a click for withdrawing and a review card for
    /// changing the traveler, for exercising the card machinery; the sample itself
    /// asks for neither.
    #[must_use]
    pub const fn with_cards(mut self) -> Self {
        self.with_cards = true;
        self
    }

    /// The phase a stored status projects to.
    #[must_use]
    pub fn phase_of(state: &TripState) -> TripPhase {
        match state.status {
            TripStatus::Draft => TripPhase::Collecting,
            TripStatus::AwaitingRebookingConfirmation => TripPhase::AwaitingRebookingConfirmation,
            TripStatus::Rebooking => TripPhase::Dispatching,
            TripStatus::Ticketed => TripPhase::Ticketed,
            TripStatus::Refused => TripPhase::Refused,
            TripStatus::Notified => TripPhase::Notified,
            TripStatus::NotNotified => TripPhase::NotNotified,
            TripStatus::Withdrawn => TripPhase::Withdrawn,
        }
    }

    fn compile_operation(
        state: Option<&TripState>,
        operation: &OperationKey,
        arguments: &serde_json::Value,
    ) -> Result<Vec<TripCommand>, DomainRejection> {
        let command = match operation.as_str() {
            operations::OPEN => {
                let args: OpenArgs = if arguments.is_null() {
                    OpenArgs::default()
                } else {
                    parse(arguments)?
                };
                match args.traveler {
                    Some(record) => TripCommand::OpenFor {
                        traveler: traveler_of(record),
                    },
                    None => TripCommand::Open,
                }
            }
            operations::SET_NAME => {
                let value = parse::<SetNameArgs>(arguments)?.value;
                // A domain rule refusing with copy of its own, so the kit exercises
                // the path a refusal takes to the user.
                if value.trim().is_empty() {
                    return Err(reject(rejection::EMPTY_NAME).with_explanation(
                        LocalizedText::new("A trip name cannot be empty.").with(
                            Locale::from("it-IT"),
                            "Il nome del viaggio non può essere vuoto.",
                        ),
                    ));
                }
                // Already the name on record: a valid act that changes nothing,
                // said by compiling nothing.
                if state.is_some_and(|state| state.name.as_deref() == Some(value.as_str())) {
                    return Ok(Vec::new());
                }
                TripCommand::SetName { value }
            }
            operations::SET_TRAVEL_DATE => TripCommand::SetTravelDate {
                value: parse::<SetTravelDateArgs>(arguments)?.value,
            },
            operations::ADD_EXTRA => {
                let args = parse::<AddExtraArgs>(arguments)?;
                if args.unit_price.currency != "EUR" {
                    return Err(reject(rejection::CURRENCY_NOT_ACCEPTED)
                        .on_argument("/unit_price")
                        .with_explanation(
                            LocalizedText::new("Extras on this desk are in euros.")
                                .with(Locale::from("it-IT"), "Gli extra qui sono in euro."),
                        ));
                }
                TripCommand::AddExtra {
                    extra: NewExtra {
                        description: args.description,
                        quantity: args.quantity,
                        unit_price_cents: args.unit_price.minor,
                    },
                }
            }
            operations::ASSIGN_PAYER => {
                let args = parse::<AssignPayerArgs>(arguments)?;
                TripCommand::AssignPayer {
                    extra_id: numbered_extra(state, args.extra)?,
                    payer: args.payer,
                }
            }
            operations::CHANGE_EXTRA => {
                let args = parse::<ChangeExtraArgs>(arguments)?;
                let extra_id = numbered_extra(state, args.extra)?;
                // Naming an extra and nothing to change on it changes nothing.
                if args.description.is_none()
                    && args.quantity.is_none()
                    && args.unit_price.is_none()
                {
                    return Ok(Vec::new());
                }
                TripCommand::ChangeExtra {
                    extra_id,
                    description: args.description,
                    quantity: args.quantity,
                    unit_price_cents: args.unit_price.map(|price| price.minor),
                }
            }
            operations::CHANGE_TRAVELER => {
                let traveler = parse::<ChangeTravelerArgs>(arguments)?.traveler;
                TripCommand::ChangeTraveler {
                    traveler: TripTraveler {
                        traveler_id: turnframe_core::hash::derive_uuid(
                            "turnframe.sample.trip.traveler",
                            &[traveler.trim()],
                        ),
                        display_name: traveler.trim().to_owned(),
                    },
                }
            }
            operations::SET_TRAVELER => TripCommand::ChangeTraveler {
                traveler: traveler_of(parse::<SetTravelerArgs>(arguments)?.traveler),
            },
            operations::PROTECT_LEG => TripCommand::ProtectLeg {
                leg: parse::<LegArgs>(arguments)?.leg,
            },
            operations::REQUEST_REBOOKING => TripCommand::RequestRebooking {
                leg: parse::<LegArgs>(arguments)?.leg,
            },
            operations::REBOOK => TripCommand::Rebook,
            operations::WITHDRAW => TripCommand::Withdraw,
            // Acknowledging the card on screen changes nothing, said by compiling
            // nothing.
            operations::ACKNOWLEDGE_CARD => return Ok(Vec::new()),
            // The reserved code, not this workflow's own, so the state explorer can
            // report a catalog that offers what the compiler does not know.
            _ => return Err(reject(turnframe_core::error::UNKNOWN_OPERATION)),
        };
        Ok(vec![command])
    }
}

fn parse<T: DeserializeOwned>(arguments: &serde_json::Value) -> Result<T, DomainRejection> {
    serde_json::from_value(arguments.clone()).map_err(|_| reject(rejection::INVALID_ARGUMENTS))
}

/// The extra a user's number names, as the record lists it from 1.
fn numbered_extra(state: Option<&TripState>, number: u32) -> Result<uuid::Uuid, DomainRejection> {
    number
        .checked_sub(1)
        .and_then(|index| state?.extras.get(usize::try_from(index).ok()?))
        .map(|extra| extra.extra_id)
        .ok_or_else(|| {
            reject(rejection::UNKNOWN_EXTRA)
                .on_argument("/extra")
                .with_explanation(
                    LocalizedText::new("This trip has no extra with that number.").with(
                        Locale::from("it-IT"),
                        "Questo viaggio non ha un extra con quel numero.",
                    ),
                )
        })
}

fn operation(key: &str, summary: &str, target: TargetPolicy) -> OperationSpec {
    OperationSpec::new(key)
        .summary(summary)
        .target(target)
        .mutating()
}

/// The argument naming a leg, by its number in the booking.
fn leg_argument(spec: OperationSpec) -> OperationSpec {
    spec.arguments::<LegArgs>().argument("leg", |a| {
        a.label("leg number")
            .label_in("it-IT", "numero della tratta")
            .describe("The leg's number as the record lists it: the outbound is 1.")
            .required()
            .inferred()
    })
}

/// The operations that edit a case. Offered while it is editable, including in
/// `AwaitingRebookingConfirmation`, where an edit takes the pending card down
/// instead of being silently refused.
fn editing_operations() -> Vec<OperationSpec> {
    let existing = TargetPolicy::RequiresExistingCase;
    vec![
        operation(
            operations::SET_NAME,
            "Name the trip: what the traveler calls it, in a few words.",
            existing,
        )
        .arguments::<SetNameArgs>()
        .argument("value", |a| {
            a.label("trip name")
                .label_in("it-IT", "nome del viaggio")
                .required()
        })
        .example(
            "the trip is for the Lisbon offsite",
            serde_json::json!({ "value": "Lisbon offsite" }),
        )
        .example(
            "chiamalo offsite Lisbona",
            serde_json::json!({ "value": "offsite Lisbona" }),
        )
        .example_not_given("the trip name needs changing", ["value"])
        .example_not_given("can I name the trip?", ["value"]),
        operation(
            operations::SET_TRAVEL_DATE,
            "Set the day the traveler would rather fly.",
            existing,
        )
        .arguments::<SetTravelDateArgs>()
        .argument("value", |a| {
            a.label("travel date")
                .label_in("it-IT", "data del viaggio")
                .required()
                .date_direction(DateDirection::Future)
        })
        .example_not_given("I want to change the travel date", ["value"]),
        operation(
            operations::ADD_EXTRA,
            "Add one extra to the trip, what it is, how many and at what price; each extra is added on its own.",
            existing,
        )
        .arguments::<AddExtraArgs>()
        .argument("description", |a| {
            a.label("description")
                .label_in("it-IT", "descrizione")
                .required()
        })
        .argument("quantity", |a| {
            a.label("quantity")
                .label_in("it-IT", "quantità")
                .describe("How many: the count this extra's own words give; «an extra» and «a bag» are one.")
                .required()
                .inferred()
        })
        .argument("unit_price", |a| {
            a.label("unit price")
                .label_in("it-IT", "prezzo unitario")
                .required()
                .money()
        })
        .example(
            "40 euros for a checked bag",
            serde_json::json!({
                "description": "checked bag",
                "quantity": 1,
                "unit_price": { "minor": 4000, "currency": "EUR" }
            }),
        )
        .example(
            "2 airport meals at 15 euros each",
            serde_json::json!({
                "description": "airport meals",
                "quantity": 2,
                "unit_price": { "minor": 1500, "currency": "EUR" }
            }),
        ),
        operation(
            operations::ASSIGN_PAYER,
            "Say who pays for one extra: the traveler, the company, or the airline.",
            existing,
        )
        .arguments::<AssignPayerArgs>()
        .argument("extra", |a| {
            a.label("extra number")
                .label_in("it-IT", "numero dell'extra")
                .describe("The extra's number as the record lists it.")
                .required()
                .inferred()
        })
        .argument("payer", |a| {
            a.label("who pays").label_in("it-IT", "chi paga").required()
        }),
        operation(
            operations::CHANGE_EXTRA,
            "Change an extra already on the trip: its description, quantity or unit price.",
            existing,
        )
        .arguments::<ChangeExtraArgs>()
        .argument("extra", |a| {
            a.label("extra number")
                .label_in("it-IT", "numero dell'extra")
                .describe("The extra's number as the record lists it.")
                .required()
                .inferred()
        })
        .argument("description", |a| {
            a.label("description").label_in("it-IT", "descrizione")
        })
        .argument("quantity", |a| {
            a.label("quantity").label_in("it-IT", "quantità")
        })
        .argument("unit_price", |a| {
            a.label("unit price")
                .label_in("it-IT", "prezzo unitario")
                .money()
        })
        .example(
            "extra 2 is actually 3 bags",
            serde_json::json!({ "extra": 2, "quantity": 3 }),
        ),
        operation(
            operations::CHANGE_TRAVELER,
            "Put another traveler on the trip.",
            existing,
        )
        .arguments::<ChangeTravelerArgs>()
        .argument("traveler", |a| {
            a.label("traveler")
                .label_in("it-IT", "viaggiatore")
                .required()
        })
        .example(
            "Tom Becker is flying instead",
            serde_json::json!({ "traveler": "Tom Becker" }),
        ),
        leg_argument(operation(
            operations::PROTECT_LEG,
            "Lock one leg of the booking so nothing changes it, asked on its own; «don't touch the return» said while the outbound changes is a condition of the turn, not a lock.",
            existing,
        ))
        .example(
            "don't touch the return flight",
            serde_json::json!({ "leg": 2 }),
        ),
    ]
}

/// A name that ends a sentence, without a full stop of its own beside the sentence's.
fn sentence_end(name: &str) -> &str {
    name.trim_end_matches('.')
}

/// The traveler a record argument names, under the name it was shown by.
fn traveler_of(record: TravelerRecord) -> TripTraveler {
    TripTraveler {
        traveler_id: turnframe_core::hash::derive_uuid(
            "turnframe.sample.trip.traveler_record",
            &[record.case_id.as_str()],
        ),
        // A traveler registered in the same message has no name yet.
        display_name: record
            .label
            .unwrap_or_else(|| "the new traveler".to_owned()),
    }
}

/// Choosing the traveler of a case that has none, from the traveler records.
fn set_traveler() -> OperationSpec {
    operation(
        operations::SET_TRAVELER,
        "Choose the traveler the trip is for.",
        TargetPolicy::RequiresExistingCase,
    )
    .arguments::<SetTravelerArgs>()
    .argument("traveler", |a| {
        a.label("traveler")
            .label_in("it-IT", "viaggiatore")
            .required()
            .record("traveler")
    })
}

/// An amount in euro cents as a reader writes it: `€84.00`, or `84,00 €` in Italian.
fn money(cents: i64, language: &str) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let (whole, part) = (cents.unsigned_abs() / 100, cents.unsigned_abs() % 100);
    match language {
        "it" => format!("{sign}{whole},{part:02} €"),
        _ => format!("{sign}€{whole}.{part:02}"),
    }
}

/// A payer as a sentence names it.
fn payer_name(payer: Payer, language: &str) -> &'static str {
    let italian = language == "it";
    match payer {
        Payer::Traveler if italian => "il viaggiatore",
        Payer::Traveler => "the traveler",
        Payer::Company if italian => "l'azienda",
        Payer::Company => "the company",
        Payer::Airline if italian => "la compagnia aerea",
        Payer::Airline => "the airline",
    }
}

/// Body copy of the rebooking card.
fn rebooking_card_body(state: &TripState) -> LocalizedText {
    let Some(offer) = &state.offer else {
        return LocalizedText::new("No rebooking is quoted.")
            .with("it", "Nessun cambio è proposto.");
    };
    let route = state
        .leg(offer.leg)
        .map_or_else(String::new, |leg| format!(" ({}→{})", leg.from, leg.to));
    let fare = offer.fare_difference_cents;
    LocalizedText::new(format!(
        "Leg {}{route} on {}, {}, {} more. Once sent, the airline decides.",
        offer.leg,
        offer.flight,
        offer.departs,
        money(fare, "en")
    ))
    .with(
        "it",
        format!(
            "Tratta {}{route} sul volo {}, {}, {} in più. Una volta inviato, decide la \
             compagnia aerea.",
            offer.leg,
            offer.flight,
            offer.departs,
            money(fare, "it")
        ),
    )
}

/// The blocking rebooking confirmation of the `AwaitingRebookingConfirmation` phase.
fn rebooking_requirement(state: &TripState) -> InteractionRequirement {
    let payload = InteractionPayload::new(
        LocalizedText::new("Rebook this flight?").with("it", "Cambio questo volo?"),
    )
    .with_body(rebooking_card_body(state))
    .with_option(
        InteractionOption::new(
            REBOOK_CONFIRM_OPTION,
            LocalizedText::new("Confirm").with("it", "Conferma"),
            StoredInteractionAction::ApplyOperation {
                operation: OperationKey::from(operations::REBOOK),
                arguments: serde_json::Value::Null,
                freeform_argument: None,
            },
        )
        .with_style(OptionStyle::Primary),
    )
    .with_option(
        InteractionOption::new(
            REBOOK_DECLINE_OPTION,
            LocalizedText::new("Keep my flight").with("it", "Tengo il mio volo"),
            // Still a refusal, nothing pending runs, and the case is told it was
            // asked, so the phase's own instruction is not served again to someone
            // who just answered it.
            StoredInteractionAction::DeclineAndRecord {
                operation: OperationKey::from(operations::ACKNOWLEDGE_CARD),
            },
        )
        .with_style(OptionStyle::Danger),
    );
    InteractionRequirement::blocking(REBOOKING_CONFIRMATION_KEY, InteractionKind::ConfirmCommand)
        .with_confirms_risk(RiskClass::ExternalRegulated)
        .with_payload(payload)
}

fn airline_notice(state: &TripState) -> Option<WorkflowNotice> {
    let status = state.external_status?;
    Some(WorkflowNotice {
        code: "trip.airline_status".to_owned(),
        severity: NoticeSeverity::Info,
        text: LocalizedText::new(format!("Airline status: {status:?}.")).with(
            "it",
            format!("Stato presso la compagnia aerea: {status:?}."),
        ),
    })
}

fn refusal_notice(state: &TripState) -> WorkflowNotice {
    let code = state.refusal_code.as_deref().unwrap_or("unknown");
    WorkflowNotice {
        code: "trip.refused_by_airline".to_owned(),
        severity: NoticeSeverity::Warning,
        text: LocalizedText::new(format!(
            "The airline refused the rebooking (code {code}). Another can be asked for."
        ))
        .with(
            "it",
            format!(
                "La compagnia aerea ha rifiutato il cambio (codice {code}). Se ne può chiedere \
                 un altro."
            ),
        ),
    }
}

impl WorkflowDefinition for TripWorkflow {
    type State = TripState;
    type Phase = TripPhase;
    type Obligation = TripObligation;
    type Command = TripCommand;
    type Event = TripEvent;
    type Outcome = TripOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from("trip")
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &TripPhase) -> PhaseOwnership {
        match phase {
            TripPhase::PreDraft | TripPhase::Collecting | TripPhase::Refused => {
                PhaseOwnership::System
            }
            TripPhase::AwaitingRebookingConfirmation => PhaseOwnership::User,
            TripPhase::Dispatching | TripPhase::Ticketed => PhaseOwnership::External,
            TripPhase::Notified | TripPhase::NotNotified | TripPhase::Withdrawn => {
                PhaseOwnership::Terminal
            }
        }
    }

    /// Each obligation as the question that closes it. An extra's payer is asked of
    /// that extra: the answer gives only the payer.
    fn obligation_act(
        &self,
        state: Option<&TripState>,
        obligation: &TripObligation,
    ) -> Option<turnframe_core::flow::ObligationAct> {
        let TripObligation::AssignPayer { extra_id } = obligation else {
            return None;
        };
        let position = state?
            .extras
            .iter()
            .position(|extra| extra.extra_id == *extra_id)?;
        Some(
            turnframe_core::flow::ObligationAct::new(operations::ASSIGN_PAYER, ["payer"])
                .given("extra", serde_json::json!(position + 1)),
        )
    }

    fn obligation_sentence(&self, obligation: &TripObligation) -> Option<LocalizedText> {
        let it = Locale::from("it-IT");
        Some(match obligation {
            TripObligation::SelectTraveler => {
                LocalizedText::new("Who is travelling?").with(it, "Chi viaggia?")
            }
            TripObligation::AssignPayer { .. } => {
                LocalizedText::new("An extra has no payer yet: who pays for it?")
                    .with(it, "Un extra non ha ancora chi lo paga: chi lo paga?")
            }
            TripObligation::SetName => LocalizedText::new("What should I call this trip?")
                .with(it, "Come chiamo questo viaggio?"),
            TripObligation::SetTravelDate => LocalizedText::new("Which day would you rather fly?")
                .with(it, "In che giorno preferisci volare?"),
        })
    }

    /// The values a person may be told back, which here is what they gave and the
    /// booking they gave it on; never the lifecycle or the ticket number.
    fn narratable_state(&self, state: Option<&TripState>) -> Vec<StateField> {
        let Some(state) = state else {
            return Vec::new();
        };
        let mut held = Vec::new();
        if let Some(traveler) = &state.traveler {
            held.push(
                StateField::new("traveler", serde_json::json!(traveler.display_name)).identifying(),
            );
        }
        if let Some(name) = &state.name {
            held.push(StateField::new("name", serde_json::json!(name)).identifying());
        }
        if let Some(date) = state.travel_date {
            held.push(StateField::new(
                "travel_date",
                serde_json::json!(date.to_string()),
            ));
        }
        if !state.legs.is_empty() {
            let legs: Vec<String> = state
                .legs
                .iter()
                .map(|leg| {
                    let kept = if leg.protected { ", kept as it is" } else { "" };
                    format!(
                        "{}. {} {}→{} {}, {:?}{kept}",
                        leg.number, leg.flight, leg.from, leg.to, leg.departs, leg.status
                    )
                })
                .collect();
            held.push(StateField::new("legs", serde_json::json!(legs)));
        }
        if !state.extras.is_empty() {
            let extras: Vec<String> = state
                .extras
                .iter()
                .enumerate()
                .map(|(index, extra)| {
                    let payer = extra
                        .payer
                        .map_or_else(|| "no payer yet".to_owned(), |p| format!("paid by {p:?}"));
                    format!(
                        "{}. {} × {} at {}.{:02} EUR, {payer}",
                        index + 1,
                        extra.quantity,
                        extra.description,
                        extra.unit_price_cents / 100,
                        extra.unit_price_cents.rem_euclid(100)
                    )
                })
                .collect();
            held.push(StateField::new("extras", serde_json::json!(extras)));
        }
        if let Some(offer) = &state.offer {
            held.push(StateField::new(
                "offer",
                serde_json::json!(format!(
                    "leg {} on {} {}, {}.{:02} EUR more",
                    offer.leg,
                    offer.flight,
                    offer.departs,
                    offer.fare_difference_cents / 100,
                    offer.fare_difference_cents.rem_euclid(100)
                )),
            ));
        }
        held
    }

    fn project(&self, case_ref: CaseRef, state: Option<&TripState>) -> ViewOf<Self> {
        let version = self.version();
        let Some(state) = state else {
            return WorkflowView::new(case_ref, version, TripPhase::PreDraft);
        };
        let phase = Self::phase_of(state);
        let view =
            WorkflowView::new(case_ref, version, phase).with_obligations(state.open_obligations());
        match phase {
            TripPhase::AwaitingRebookingConfirmation => {
                view.with_blocking_interaction(rebooking_requirement(state))
            }
            TripPhase::Refused => view.with_notice(refusal_notice(state)),
            TripPhase::Dispatching | TripPhase::Ticketed => match airline_notice(state) {
                Some(notice) => view.with_notice(notice),
                None => view,
            },
            TripPhase::Withdrawn => view.with_outcome(TripOutcome::Withdrawn),
            TripPhase::Notified => view.with_outcome(TripOutcome::Notified),
            TripPhase::NotNotified => view.with_outcome(TripOutcome::NotNotified),
            TripPhase::PreDraft | TripPhase::Collecting => view,
        }
    }

    fn summary(&self) -> Option<String> {
        Some(String::from(
            "Trips: the disruption case of a booking, from its extras to a rebooking.",
        ))
    }

    fn noun(&self) -> Option<LocalizedText> {
        Some(LocalizedText::new("trip").with("it", "viaggio"))
    }

    fn glossary(&self) -> Vec<GlossaryTerm> {
        vec![GlossaryTerm::new(
            "fare difference",
            "what the new flight costs beyond the ticket already paid",
        )]
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        super::super::in_italian(Self::offered(view), ITALIAN)
    }

    /// Guidance that changes with the phase: while a case is being filled in, what
    /// a kept leg means; once the rebooking card is up, to read the message as an
    /// answer to it.
    fn briefing(&self, view: &ViewOf<Self>) -> Option<String> {
        match view.phase {
            TripPhase::Collecting => Some(String::from(
                "A leg marked as kept is one the traveler asked to leave as it is: never \
                 propose changing it. A field that is not open as an obligation has already \
                 been settled; do not ask for it again.",
            )),
            TripPhase::AwaitingRebookingConfirmation => Some(String::from(
                "This trip is waiting on the rebooking card. Read the message as an answer to \
                 that card, not as a new instruction, unless it plainly changes a field.",
            )),
            _ => None,
        }
    }

    /// What the sample wants said while it acknowledges and asks.
    fn transition_briefing(&self, view: &ViewOf<Self>) -> Option<String> {
        match view.phase {
            TripPhase::PreDraft => Some(String::from(
                "There is no case yet. What is needed first is who is travelling: ask for the \
                 traveler and nothing else.",
            )),
            TripPhase::Collecting => Some(String::from(
                "Ask for one thing at a time, in the order the obligations are listed. Do not \
                 read the trip back unless the user asked for it.",
            )),
            TripPhase::AwaitingRebookingConfirmation => Some(String::from(
                "The rebooking card below your text already shows the flight and the fare. Do \
                 not repeat them.",
            )),
            _ => None,
        }
    }

    /// And something else for the stage that answers.
    fn answer_briefing(&self, view: &ViewOf<Self>) -> Option<String> {
        match view.phase {
            TripPhase::Collecting => Some(String::from(
                "Amounts on this trip are in whole cents; say them as currency.",
            )),
            _ => None,
        }
    }

    /// The itinerary a quoted rebooking has, declared from the projection so it is
    /// there on a turn where the user asks instead of clicking.
    fn artifacts(&self, view: &ViewOf<Self>) -> Vec<ArtifactRef> {
        if !matches!(
            view.phase,
            TripPhase::AwaitingRebookingConfirmation | TripPhase::Ticketed
        ) {
            return Vec::new();
        }
        vec![ArtifactRef {
            artifact_id: format!("itinerary:{}", view.case_ref.case_id),
            kind: "itinerary_pdf".to_owned(),
            label: LocalizedText::new("Itinerary preview").with("it", "Anteprima itinerario"),
            uri: None,
            media_type: Some("application/pdf".to_owned()),
        }]
    }

    /// What the destructive confirmation is about, in words that name the thing.
    fn confirmation_subject(
        &self,
        state: Option<&TripState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Option<ConfirmationSubject> {
        let ResolvedActKind::ApplyOperation { operation } = &act.kind else {
            return None;
        };
        if operation.as_str() != operations::WITHDRAW {
            return None;
        }
        let who = state.and_then(|state| state.traveler.as_ref()).map_or_else(
            || String::from("this case"),
            |traveler| format!("the case for {}", traveler.display_name),
        );
        Some(ConfirmationSubject::asking(LocalizedText::new(format!(
            "Withdraw {who}?"
        ))))
    }

    /// The payers this workflow accepts, and no others.
    fn enumerations(&self, _view: &ViewOf<Self>) -> Vec<DomainEnumeration> {
        vec![
            DomainEnumeration::new(
                "extras.payer",
                vec![
                    EnumeratedValue::new("traveler", LocalizedText::new("The traveler")),
                    EnumeratedValue::new("company", LocalizedText::new("The company")),
                    EnumeratedValue::new("airline", LocalizedText::new("The airline")),
                ],
            )
            .with_preamble(LocalizedText::new("An extra can be paid by one of these.")),
        ]
    }

    /// A complete trip can take another extra or be rebooked when a quote is in;
    /// one still owing something offers nothing, because what it owes is asked first.
    fn next_steps(&self, view: &ViewOf<Self>) -> Vec<LocalizedText> {
        if view.phase != TripPhase::Collecting || !view.obligations.is_empty() {
            return Vec::new();
        }
        vec![
            LocalizedText::new("Add another extra.").with("it", "Aggiungere un altro extra."),
            LocalizedText::new("Rebook the quoted flight: a card asks to confirm first.").with(
                "it",
                "Cambiare il volo proposto: prima una scheda chiede conferma.",
            ),
        ]
    }

    fn compile_act(
        &self,
        state: Option<&TripState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<TripCommand>, DomainRejection> {
        match &act.kind {
            ResolvedActKind::StartWorkflow => Ok(vec![TripCommand::Open]),
            ResolvedActKind::ApplyOperation { operation } => {
                Self::compile_operation(state, operation, &act.arguments)
            }
            _ => Err(reject(rejection::UNSUPPORTED_ACT)),
        }
    }

    /// Why nothing changed: the name already there, and what to say next.
    fn nothing_changed(
        &self,
        state: Option<&TripState>,
        act: &ResolvedAct,
    ) -> Option<LocalizedText> {
        let ResolvedActKind::ApplyOperation { operation } = &act.kind else {
            return None;
        };
        if operation.as_str() != operations::SET_NAME {
            return None;
        }
        let name = state?.name.as_deref()?;
        Some(
            LocalizedText::new(format!(
                "The trip is already called \"{name}\". Say which name you want instead."
            ))
            .with(
                Locale::from("it-IT"),
                format!("Il viaggio si chiama già «{name}». Dimmi quale nome vuoi."),
            ),
        )
    }

    fn command_policy(&self, state: Option<&TripState>, command: &TripCommand) -> CommandPolicy {
        match command {
            // Choosing the first traveler replaces nobody: it needs no review.
            TripCommand::ChangeTraveler { .. }
                if !self.with_cards || state.is_some_and(|state| state.traveler.is_none()) =>
            {
                CommandPolicy::low_risk()
            }
            TripCommand::Withdraw if !self.with_cards => CommandPolicy::low_risk(),
            TripCommand::Rebook => CommandPolicy {
                risk: RiskClass::ExternalRegulated,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::ExternalSaga {
                    saga: "trip.rebooking".to_owned(),
                },
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            TripCommand::Withdraw => CommandPolicy {
                risk: RiskClass::Destructive,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            TripCommand::ChangeTraveler { .. } => CommandPolicy {
                risk: RiskClass::SensitiveDataChange,
                confirmation: ConfirmationPolicy::ReviewCard,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::EventReferencedParaphrase,
            },
            TripCommand::RecordAirlineOutcome { .. } => CommandPolicy {
                risk: RiskClass::ExternalRegulated,
                confirmation: ConfirmationPolicy::None,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            TripCommand::Open
            | TripCommand::OpenFor { .. }
            | TripCommand::SetName { .. }
            | TripCommand::SetTravelDate { .. }
            | TripCommand::AddExtra { .. }
            | TripCommand::AssignPayer { .. }
            | TripCommand::ChangeExtra { .. }
            | TripCommand::ProtectLeg { .. }
            | TripCommand::Requote { .. }
            | TripCommand::RequestRebooking { .. } => CommandPolicy::low_risk(),
        }
    }

    fn validate_command(
        &self,
        state: Option<&TripState>,
        command: &TripCommand,
    ) -> Result<(), DomainRejection> {
        validate(state, command)
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<TripEvent>],
        locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        let _ = locale;
        events
            .iter()
            .map(|event| match event {
                ReceiptEvent::Committed(committed) => committed_receipt(committed),
                ReceiptEvent::Redacted(redacted) => redacted_receipt(redacted),
            })
            .collect()
    }

    fn build_interaction(
        &self,
        state: Option<&TripState>,
        view: &ViewOf<Self>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, DomainRejection> {
        let mut spec = requirement.to_spec(view.case_ref.clone());
        if let Some(state) = state {
            // The card names what it was drawn from: a new quote changes both.
            let preview =
                canonical_digest(state).map_err(|_| reject(rejection::INVALID_ARGUMENTS))?;
            spec.payload = spec.payload.with_metadata(serde_json::json!({
                "preview_hash": preview.as_str(),
                "bound_revision": view.case_ref.expected_revision.value(),
            }));
        }
        Ok(spec)
    }
}

impl TripWorkflow {
    /// The operations a view offers, with their summaries in English.
    fn offered(view: &ViewOf<Self>) -> Vec<OperationSpec> {
        // Withdrawing takes only a record already in view: the sample's coverage of
        // `RequiresCatalogedCase`.
        let withdraw = operation(
            operations::WITHDRAW,
            "Withdraw the case before a rebooking is sent.",
            TargetPolicy::RequiresCatalogedCase,
        );
        let request_rebooking = leg_argument(operation(
            operations::REQUEST_REBOOKING,
            "Show the rebooking card for the flight the airline quoted for a leg.",
            TargetPolicy::RequiresExistingCase,
        ));
        match view.phase {
            TripPhase::PreDraft => vec![
                operation(
                    operations::OPEN,
                    "Open a new trip, the disruption case of one booking, for its traveler when named.",
                    TargetPolicy::NewCaseOnly,
                )
                .arguments::<OpenArgs>()
                .argument("traveler", |a| {
                    a.label("traveler")
                        .label_in("it-IT", "viaggiatore")
                        .record("traveler")
                }),
            ],
            TripPhase::Collecting | TripPhase::Refused => {
                let mut offered = editing_operations();
                // The first traveler is chosen from the traveler records; changing it
                // later is the reviewed change.
                if view.obligations.contains(&TripObligation::SelectTraveler) {
                    offered.retain(|spec| spec.key.as_str() != operations::CHANGE_TRAVELER);
                    offered.push(set_traveler());
                }
                offered.push(request_rebooking);
                offered.push(withdraw);
                offered
            }
            TripPhase::AwaitingRebookingConfirmation => {
                let mut offered = editing_operations();
                // Only the card's own button runs this: a user asking to see the
                // confirmation again gets the recap, not a bare "Confirm?".
                offered.push(
                    operation(
                        operations::REBOOK,
                        "Send the rebooking to the airline.",
                        TargetPolicy::RequiresCatalogedCase,
                    )
                    .card_only(),
                );
                // The card's «Keep my flight» records that it was asked; only its
                // button runs it.
                offered.push(
                    operation(
                        operations::ACKNOWLEDGE_CARD,
                        "Acknowledge the card currently on screen.",
                        TargetPolicy::RequiresCatalogedCase,
                    )
                    .card_only(),
                );
                offered.push(withdraw);
                offered
            }
            TripPhase::Dispatching
            | TripPhase::Ticketed
            | TripPhase::Notified
            | TripPhase::NotNotified
            | TripPhase::Withdrawn => Vec::new(),
        }
    }
}

/// What each operation does, in Italian.
const ITALIAN: &[(&str, &str)] = &[
    (
        operations::OPEN,
        "Apre un nuovo viaggio, la pratica di una prenotazione, per il viaggiatore se è nominato.",
    ),
    (
        operations::SET_NAME,
        "Dà un nome al viaggio: come lo chiama il viaggiatore, in poche parole.",
    ),
    (
        operations::SET_TRAVEL_DATE,
        "Imposta il giorno in cui il viaggiatore preferisce volare.",
    ),
    (
        operations::ADD_EXTRA,
        "Aggiunge un extra al viaggio, cos'è, quanti e a che prezzo; ogni extra si aggiunge da solo.",
    ),
    (
        operations::ASSIGN_PAYER,
        "Dice chi paga un extra: il viaggiatore, l'azienda o la compagnia aerea.",
    ),
    (
        operations::CHANGE_EXTRA,
        "Cambia un extra già nel viaggio: descrizione, quantità o prezzo unitario.",
    ),
    (
        operations::SET_TRAVELER,
        "Sceglie il viaggiatore della pratica.",
    ),
    (
        operations::CHANGE_TRAVELER,
        "Mette un altro viaggiatore sul viaggio.",
    ),
    (
        operations::PROTECT_LEG,
        "Blocca una tratta della prenotazione perché niente la cambi, chiesto da solo; «non toccare il ritorno» detto mentre si cambia l'andata è una condizione del turno, non un blocco.",
    ),
    (
        operations::REQUEST_REBOOKING,
        "Mostra la scheda di cambio per il volo proposto dalla compagnia aerea su una tratta.",
    ),
    (operations::REBOOK, "Invia il cambio alla compagnia aerea."),
    (
        operations::WITHDRAW,
        "Ritira la pratica prima che un cambio sia inviato.",
    ),
    (
        operations::ACKNOWLEDGE_CARD,
        "Prende atto della scheda sullo schermo.",
    ),
];

/// Renders one committed event as a receipt, copying the real event id so the
/// claim is backed by the ledger.
fn committed_receipt(committed: &CommittedEvent<TripEvent>) -> OperationalReceipt {
    let event_ids = vec![committed.event_id];
    let status_code = committed.payload.event_type().to_owned();
    let (severity, title, body) = receipt_copy(&committed.payload);
    OperationalReceipt {
        receipt_id: ReceiptId::derive(&event_ids, &status_code),
        event_ids,
        severity,
        title,
        body,
        status_code,
        artifact_refs: Vec::new(),
    }
}

/// Renders an event whose payload was erased.
///
/// The receipt still cites the event, so the claim guard still backs it; what the
/// copy must not do is describe a change whose values are gone. The event type is
/// kept out of the copy too: a user is owed a sentence, not a symbol.
fn redacted_receipt(redacted: &RedactedEvent) -> OperationalReceipt {
    let event_ids = vec![redacted.event_id];
    let status_code = "trip.detail_erased".to_owned();
    OperationalReceipt {
        receipt_id: ReceiptId::derive(&event_ids, &status_code),
        event_ids,
        severity: ReceiptSeverity::Info,
        title: LocalizedText::new("Detail erased").with("it", "Dettaglio cancellato"),
        body: LocalizedText::new(
            "This change is still on record; the detail of what changed was erased.",
        )
        .with(
            "it",
            "Questa modifica resta a registro; il dettaglio di cosa è cambiato è stato cancellato.",
        ),
        status_code,
        artifact_refs: Vec::new(),
    }
}

/// Server-authored copy, in English with an Italian translation.
fn receipt_copy(event: &TripEvent) -> (ReceiptSeverity, LocalizedText, LocalizedText) {
    match event {
        TripEvent::Opened => (
            ReceiptSeverity::Success,
            LocalizedText::new("Case opened").with("it", "Pratica aperta"),
            LocalizedText::new("A new disruption case is open.")
                .with("it", "È aperta una nuova pratica."),
        ),
        TripEvent::NameSet { value } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Trip named").with("it", "Nome impostato"),
            LocalizedText::new(format!("The trip is now called \"{value}\"."))
                .with("it", format!("Il viaggio ora si chiama \"{value}\".")),
        ),
        TripEvent::TravelDateSet { value } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Travel date set").with("it", "Data impostata"),
            LocalizedText::new(format!("The traveler would rather fly on {value}.")).with(
                "it",
                format!("Il viaggiatore preferisce volare il {value}."),
            ),
        ),
        TripEvent::ExtraAdded {
            description,
            quantity,
            unit_price_cents,
            ..
        } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Extra added").with("it", "Extra aggiunto"),
            LocalizedText::new(format!(
                "{quantity} x \"{description}\" at {} each.",
                money(*unit_price_cents, "en")
            ))
            .with(
                "it",
                format!(
                    "{quantity} x \"{description}\" a {} l'uno.",
                    money(*unit_price_cents, "it")
                ),
            ),
        ),
        TripEvent::PayerAssigned { payer, .. } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Payer set").with("it", "Pagante impostato"),
            LocalizedText::new(format!("Paid by {}.", payer_name(*payer, "en")))
                .with("it", format!("Lo paga {}.", payer_name(*payer, "it"))),
        ),
        TripEvent::ExtraChanged {
            description,
            quantity,
            unit_price_cents,
            ..
        } => {
            let mut english = Vec::new();
            let mut italian = Vec::new();
            if let Some(description) = description {
                english.push(format!("described as \"{description}\""));
                italian.push(format!("descritto come \"{description}\""));
            }
            if let Some(quantity) = quantity {
                english.push(format!("quantity {quantity}"));
                italian.push(format!("quantità {quantity}"));
            }
            if let Some(cents) = unit_price_cents {
                english.push(format!("{} each", money(*cents, "en")));
                italian.push(format!("{} l'uno", money(*cents, "it")));
            }
            (
                ReceiptSeverity::Success,
                LocalizedText::new("Extra changed").with("it", "Extra modificato"),
                LocalizedText::new(format!("The extra is now {}.", english.join(", ")))
                    .with("it", format!("L'extra ora è {}.", italian.join(", "))),
            )
        }
        TripEvent::TravelerChanged {
            previous_traveler_id: None,
            display_name,
            ..
        } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Traveler set").with("it", "Viaggiatore impostato"),
            LocalizedText::new(format!("The trip is for {}.", sentence_end(display_name))).with(
                "it",
                format!("Il viaggio è per {}.", sentence_end(display_name)),
            ),
        ),
        TripEvent::TravelerChanged { display_name, .. } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Traveler changed").with("it", "Viaggiatore cambiato"),
            LocalizedText::new(format!(
                "{} is now the traveler.",
                sentence_end(display_name)
            ))
            .with("it", format!("Ora viaggia {}.", sentence_end(display_name))),
        ),
        TripEvent::LegProtected { leg } => (
            ReceiptSeverity::Success,
            LocalizedText::new("Leg kept").with("it", "Tratta bloccata"),
            LocalizedText::new(format!("Leg {leg} stays as it is; nothing will change it.")).with(
                "it",
                format!("La tratta {leg} resta com'è; niente la cambierà."),
            ),
        ),
        TripEvent::OfferQuoted {
            leg,
            flight,
            fare_difference_cents,
            ..
        } => (
            ReceiptSeverity::Info,
            LocalizedText::new("Rebooking quoted").with("it", "Cambio proposto"),
            LocalizedText::new(format!(
                "The airline quotes {flight} for leg {leg}, {} more.",
                money(*fare_difference_cents, "en")
            ))
            .with(
                "it",
                format!(
                    "La compagnia aerea propone il volo {flight} per la tratta {leg}, {} in più.",
                    money(*fare_difference_cents, "it")
                ),
            ),
        ),
        TripEvent::RebookingRequested { .. } => (
            ReceiptSeverity::Info,
            LocalizedText::new("Ready to rebook").with("it", "Pronto per il cambio"),
            LocalizedText::new("Confirm the card below to send the rebooking to the airline.")
                .with(
                    "it",
                    "Conferma la scheda qui sotto per inviare il cambio alla compagnia aerea.",
                ),
        ),
        TripEvent::RebookingSent => (
            ReceiptSeverity::Success,
            LocalizedText::new("Rebooking sent").with("it", "Cambio inviato"),
            LocalizedText::new(
                "The rebooking was sent to the airline; it is not confirmed until they answer.",
            )
            .with(
                "it",
                "Il cambio è stato inviato alla compagnia aerea; non è confermato finché non \
                 risponde.",
            ),
        ),
        TripEvent::Withdrawn => (
            ReceiptSeverity::Success,
            LocalizedText::new("Case withdrawn").with("it", "Pratica ritirata"),
            LocalizedText::new("The case was withdrawn before any rebooking was sent.").with(
                "it",
                "La pratica è stata ritirata prima di inviare un cambio.",
            ),
        ),
        TripEvent::AirlineOutcomeRecorded {
            status,
            ticket_number,
            reason_code,
        } => {
            let severity = match status {
                turnframe_core::event::ExternalStatus::Rejected
                | turnframe_core::event::ExternalStatus::NotDelivered => ReceiptSeverity::Warning,
                _ => ReceiptSeverity::Success,
            };
            let detail = ticket_number
                .as_deref()
                .or(reason_code.as_deref())
                .unwrap_or("-");
            (
                severity,
                LocalizedText::new("Airline answered").with("it", "Risposta della compagnia"),
                LocalizedText::new(format!("The airline reported {status:?} ({detail}).")).with(
                    "it",
                    format!("La compagnia aerea ha comunicato {status:?} ({detail})."),
                ),
            )
        }
    }
}

impl PureWorkflow for TripWorkflow {
    fn apply(
        &self,
        state: Option<&TripState>,
        command: &TripCommand,
    ) -> Result<Applied<TripState, TripEvent>, DomainRejection> {
        apply(state, command)
    }

    fn event_type(&self, event: &TripEvent) -> String {
        event.event_type().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn copy(event: &TripEvent, locale: &str) -> String {
        let (_, _, body) = receipt_copy(event);
        body.resolve(&Locale::new(locale)).to_owned()
    }

    #[test]
    fn an_extra_change_that_changes_nothing_compiles_to_nothing() {
        let state = crate::workflows::trip::model::unassigned_case();
        let compiled = TripWorkflow::compile_operation(
            Some(&state),
            &OperationKey::from(operations::CHANGE_EXTRA),
            &serde_json::json!({ "extra": 1 }),
        );
        assert_eq!(compiled.unwrap(), Vec::new());
    }

    #[test]
    fn an_extra_receipt_shows_its_price_as_money() {
        let event = TripEvent::ExtraAdded {
            extra_id: Uuid::nil(),
            description: "bags".to_owned(),
            quantity: 2,
            unit_price_cents: 4_050,
        };
        assert_eq!(copy(&event, "en-GB"), "2 x \"bags\" at €40.50 each.");
        assert_eq!(copy(&event, "it"), "2 x \"bags\" a 40,50 € l'uno.");
    }

    #[test]
    fn a_payer_receipt_names_the_payer() {
        let event = TripEvent::PayerAssigned {
            extra_id: Uuid::nil(),
            payer: Payer::Airline,
        };
        assert_eq!(copy(&event, "en-GB"), "Paid by the airline.");
    }

    #[test]
    fn the_rebooking_card_shows_the_quoted_fare() {
        let state = crate::workflows::trip::model::awaiting_rebooking_confirmation();
        let body = rebooking_card_body(&state);
        assert_eq!(
            body.resolve(&Locale::new("en-GB")),
            "Leg 1 (FCO→LIS) on AZ612, 2026-10-05 13:10, €84.00 more. Once sent, the airline \
             decides."
        );
    }
}
