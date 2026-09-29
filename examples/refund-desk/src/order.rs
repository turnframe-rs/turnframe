//! The `order` workflow: what a customer paid, and the refunds of it.
//!
//! A refund is asked for, shown on a card bound to the order's revision, and sent to the
//! payment provider only by that card's button. The workflow holds no clock: whether the
//! refund window is open is part of the order's state.

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use turnframe::command::{AtomicityScope, ClaimMode, CommandPolicy, ConfirmationPolicy, RiskClass};
use turnframe::error::DomainRejection;
use turnframe::event::{
    CommittedEvent, ExternalStatus, OperationalReceipt, ReceiptEvent, ReceiptSeverity,
};
use turnframe::flow::{
    CaseRef, InteractionRequirement, PhaseOwnership, StateField, ViewOf, WorkflowDefinition,
    WorkflowView,
};
use turnframe::ids::{OperationKey, ReceiptId, WorkflowKey, WorkflowVersion};
use turnframe::interaction::{
    FieldValue, InteractionKind, InteractionOption, InteractionPayload, InteractionSpec,
    OptionStyle, ReviewDiffEntry, StoredInteractionAction,
};
use turnframe::locale::{Locale, LocalizedText};
use turnframe::operation::{Money, OperationSpec};
use turnframe::plan::TargetPolicy;
use turnframe::schema::canonical_digest;
use turnframe::target::{ResolvedAct, ResolvedActKind};
use turnframe::testing::workflows::{Applied, PureWorkflow};

/// The operations of an order.
pub mod operations {
    /// Asks for a refund: records it and raises the card.
    pub const REQUEST_REFUND: &str = "order.request_refund";
    /// Sends the pending refund to the payment provider; only the card's button runs it.
    pub const REFUND: &str = "order.refund";
    /// Drops the pending refund; the card's other button.
    pub const DECLINE_REFUND: &str = "order.decline_refund";
}

/// Stable rejection codes.
pub mod rejection {
    /// The order does not exist.
    pub const NOT_FOUND: &str = "order.not_found";
    /// More than is left to refund.
    pub const EXCEEDS_PAID: &str = "order.refund_exceeds_paid";
    /// The refund window has closed.
    pub const WINDOW_CLOSED: &str = "order.window_closed";
    /// A refund is already pending or on its way.
    pub const REFUND_PENDING: &str = "order.refund_pending";
    /// Nothing is pending.
    pub const NOTHING_PENDING: &str = "order.nothing_pending";
    /// The amount is not a positive amount in euros.
    pub const INVALID_AMOUNT: &str = "order.invalid_amount";
    /// No refund is waiting for the provider's answer.
    pub const NOTHING_SENT: &str = "order.nothing_sent";
    /// The act is not one this workflow compiles.
    pub const UNSUPPORTED: &str = "order.unsupported";
}

/// Key of the blocking refund card.
pub const REFUND_CARD_KEY: &str = "order.refund_confirmation";
/// The card's option that sends the refund.
pub const REFUND_OPTION: &str = "refund";
/// The card's option that drops it.
pub const KEEP_OPTION: &str = "keep";

/// One order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderState {
    /// Its number, as the customer knows it.
    pub number: u32,
    /// Who paid.
    pub customer: String,
    /// What was paid, in euro cents.
    pub paid_cents: i64,
    /// What has been refunded.
    pub refunded_cents: i64,
    /// When it was delivered.
    pub delivered: NaiveDate,
    /// The last day a refund may be asked for.
    pub window_until: NaiveDate,
    /// Whether that window is still open: closing it is an outside event.
    pub window_open: bool,
    /// Asked for, waiting on the card.
    pub pending_cents: Option<i64>,
    /// Sent, waiting on the provider.
    pub sending_cents: Option<i64>,
    /// Where the refund stands with the payment provider.
    pub external: Option<ExternalStatus>,
    /// The provider's reference, once it answered.
    pub reference: Option<String>,
}

impl OrderState {
    /// A delivered order, nothing refunded, its window open until `window_until`.
    ///
    /// # Panics
    ///
    /// When a date is not `YYYY-MM-DD`.
    #[must_use]
    pub fn delivered(
        number: u32,
        customer: &str,
        paid_cents: i64,
        delivered: &str,
        window_until: &str,
    ) -> Self {
        let date = |text: &str| text.parse().expect("dates are written YYYY-MM-DD");
        Self {
            number,
            customer: customer.to_owned(),
            paid_cents,
            refunded_cents: 0,
            delivered: date(delivered),
            window_until: date(window_until),
            window_open: true,
            pending_cents: None,
            sending_cents: None,
            external: None,
            reference: None,
        }
    }

    /// What may still be refunded: what was paid, less what was refunded or is on its way.
    #[must_use]
    pub fn refundable_cents(&self) -> i64 {
        self.paid_cents - self.refunded_cents - self.sending_cents.unwrap_or(0)
    }

    /// The amount the card sends: the pending one, capped at what is left.
    #[must_use]
    pub fn card_cents(&self) -> Option<i64> {
        self.pending_cents
            .map(|pending| pending.min(self.refundable_cents()))
    }
}

/// A change to an order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderCommand {
    /// Ask for a refund of `cents`.
    RequestRefund {
        /// The amount.
        cents: i64,
    },
    /// Send the pending refund.
    Refund,
    /// Drop the pending refund.
    DeclineRefund,
    /// A refund made by somebody else, outside any turn.
    RefundOutside {
        /// The amount.
        cents: i64,
        /// Who made it.
        by: String,
    },
    /// The payment provider's answer, delivered outside any turn.
    RecordProviderOutcome {
        /// What the provider reported.
        status: ExternalStatus,
        /// Its reference.
        reference: Option<String>,
    },
}

/// What happened to an order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderEvent {
    /// A refund was asked for.
    RefundRequested {
        /// The amount.
        cents: i64,
    },
    /// The refund went to the payment provider.
    RefundSent {
        /// The amount sent.
        cents: i64,
    },
    /// The pending refund was dropped.
    RefundDeclined,
    /// Somebody else refunded part of the order.
    RefundedOutside {
        /// The amount.
        cents: i64,
        /// Who.
        by: String,
    },
    /// The payment provider answered.
    ProviderOutcomeRecorded {
        /// What it reported.
        status: ExternalStatus,
        /// Its reference.
        reference: Option<String>,
    },
}

impl OrderEvent {
    /// The stable type label.
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::RefundRequested { .. } => "order.refund_requested",
            Self::RefundSent { .. } => "order.refund_sent",
            Self::RefundDeclined => "order.refund_declined",
            Self::RefundedOutside { .. } => "order.refunded_outside",
            Self::ProviderOutcomeRecorded { .. } => "order.provider_outcome_recorded",
        }
    }
}

/// Where an order stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderPhase {
    /// No such order.
    Missing,
    /// Nothing pending.
    Open,
    /// A refund waits on its card.
    AwaitingConfirmation,
    /// A refund waits on the payment provider.
    Sending,
    /// All of it is refunded.
    Refunded,
}

/// An order owes nothing: every field is given when it is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderObligation {}

/// How an order ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderOutcome {
    /// All of it was refunded.
    Refunded,
}

/// Arguments of [`operations::REQUEST_REFUND`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestRefundArgs {
    /// How much to refund.
    pub amount: Money,
}

/// The `order` workflow.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrderWorkflow;

/// An amount of euro cents as the desk writes it: `€1,290.00`.
#[must_use]
pub fn euros(cents: i64) -> String {
    let whole = (cents / 100).to_string();
    let mut grouped = String::new();
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("€{grouped}.{:02}", cents.rem_euclid(100))
}

fn reject(code: &'static str) -> DomainRejection {
    let key = code.replace('.', ".error.");
    DomainRejection::new(code, key)
}

fn exceeds(state: &OrderState) -> DomainRejection {
    reject(rejection::EXCEEDS_PAID)
        .on_argument("/amount")
        .with_explanation(LocalizedText::new(format!(
            "Order {} was paid {} and {} of it is refunded: at most {} can be refunded.",
            state.number,
            euros(state.paid_cents),
            euros(state.refunded_cents),
            euros(state.refundable_cents())
        )))
}

/// Checks `command` against `state` without changing anything.
pub fn validate(state: Option<&OrderState>, command: &OrderCommand) -> Result<(), DomainRejection> {
    let Some(state) = state else {
        return Err(reject(rejection::NOT_FOUND));
    };
    match command {
        OrderCommand::RequestRefund { cents } => {
            if *cents <= 0 {
                return Err(reject(rejection::INVALID_AMOUNT).on_argument("/amount"));
            }
            if state.pending_cents.is_some() || state.sending_cents.is_some() {
                return Err(reject(rejection::REFUND_PENDING).with_explanation(
                    LocalizedText::new(format!(
                        "A refund of order {} is already waiting.",
                        state.number
                    )),
                ));
            }
            if !state.window_open {
                return Err(
                    reject(rejection::WINDOW_CLOSED).with_explanation(LocalizedText::new(format!(
                        "The refund window of order {} closed on {}.",
                        state.number, state.window_until
                    ))),
                );
            }
            if *cents > state.refundable_cents() {
                return Err(exceeds(state));
            }
            Ok(())
        }
        OrderCommand::Refund => match state.card_cents() {
            None => Err(reject(rejection::NOTHING_PENDING)),
            Some(cents) if cents <= 0 => Err(exceeds(state)),
            Some(_) => Ok(()),
        },
        OrderCommand::DeclineRefund => state
            .pending_cents
            .map(|_| ())
            .ok_or_else(|| reject(rejection::NOTHING_PENDING)),
        OrderCommand::RefundOutside { cents, .. } if *cents > state.refundable_cents() => {
            Err(exceeds(state))
        }
        OrderCommand::RefundOutside { .. } => Ok(()),
        OrderCommand::RecordProviderOutcome { .. } => state
            .sending_cents
            .map(|_| ())
            .ok_or_else(|| reject(rejection::NOTHING_SENT)),
    }
}

/// Applies `command` to `state`, refusing it as [`validate`] does.
pub fn apply(
    state: Option<&OrderState>,
    command: &OrderCommand,
) -> Result<Applied<OrderState, OrderEvent>, DomainRejection> {
    validate(state, command)?;
    let Some(state) = state else {
        return Err(reject(rejection::NOT_FOUND));
    };
    let mut next = state.clone();
    let event = match command {
        OrderCommand::RequestRefund { cents } => {
            next.pending_cents = Some(*cents);
            next.external = Some(ExternalStatus::AwaitingConfirmation);
            OrderEvent::RefundRequested { cents: *cents }
        }
        OrderCommand::Refund => {
            let cents = state.card_cents().unwrap_or(0);
            next.pending_cents = None;
            next.sending_cents = Some(cents);
            next.external = Some(ExternalStatus::Submitted);
            OrderEvent::RefundSent { cents }
        }
        OrderCommand::DeclineRefund => {
            next.pending_cents = None;
            next.external = None;
            OrderEvent::RefundDeclined
        }
        OrderCommand::RefundOutside { cents, by } => {
            next.refunded_cents += cents;
            OrderEvent::RefundedOutside {
                cents: *cents,
                by: by.clone(),
            }
        }
        OrderCommand::RecordProviderOutcome { status, reference } => {
            if *status == ExternalStatus::Accepted {
                next.refunded_cents += state.sending_cents.unwrap_or(0);
            }
            next.sending_cents = None;
            next.external = Some(*status);
            next.reference.clone_from(reference);
            OrderEvent::ProviderOutcomeRecorded {
                status: *status,
                reference: reference.clone(),
            }
        }
    };
    Ok(Applied::new(next, vec![event]))
}

fn phase_of(state: &OrderState) -> OrderPhase {
    if state.sending_cents.is_some() {
        OrderPhase::Sending
    } else if state.pending_cents.is_some() {
        OrderPhase::AwaitingConfirmation
    } else if state.refunded_cents >= state.paid_cents {
        OrderPhase::Refunded
    } else {
        OrderPhase::Open
    }
}

/// The refund card: the amount it sends, to whom, and the change when it was capped.
#[must_use]
pub fn refund_payload(state: &OrderState) -> InteractionPayload {
    let cents = state.card_cents().unwrap_or(0);
    let mut payload = InteractionPayload::new(LocalizedText::new("Refund this order?"))
        .with_body(LocalizedText::new(format!(
            "{} to {}, order {}. Once sent, the payment provider decides.",
            euros(cents),
            state.customer,
            state.number
        )))
        .with_option(
            InteractionOption::new(
                REFUND_OPTION,
                LocalizedText::new("Refund"),
                StoredInteractionAction::ApplyOperation {
                    operation: OperationKey::from(operations::REFUND),
                    arguments: serde_json::Value::Null,
                    freeform_argument: None,
                },
            )
            .with_style(OptionStyle::Primary),
        )
        .with_option(
            InteractionOption::new(
                KEEP_OPTION,
                LocalizedText::new("Don't refund"),
                StoredInteractionAction::DeclineAndRecord {
                    operation: OperationKey::from(operations::DECLINE_REFUND),
                },
            )
            .with_style(OptionStyle::Danger),
        );
    if let Some(pending) = state.pending_cents.filter(|pending| *pending != cents) {
        payload = payload.with_review_entry(
            ReviewDiffEntry::new("amount", LocalizedText::new("Amount"))
                .with_before(FieldValue::present(euros(pending)))
                .with_after(FieldValue::present(euros(cents))),
        );
    }
    payload
}

fn operation(key: &str, summary: &str) -> OperationSpec {
    OperationSpec::new(key)
        .summary(summary)
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
}

fn receipt_copy(event: &OrderEvent) -> (ReceiptSeverity, &'static str, String) {
    match event {
        OrderEvent::RefundRequested { cents } => (
            ReceiptSeverity::Info,
            "Refund requested",
            format!(
                "Confirm the card to send {} to the payment provider.",
                euros(*cents)
            ),
        ),
        OrderEvent::RefundSent { cents } => (
            ReceiptSeverity::Info,
            "Refund sent",
            format!(
                "{} was sent to the payment provider; it is not refunded until they answer.",
                euros(*cents)
            ),
        ),
        OrderEvent::RefundDeclined => (
            ReceiptSeverity::Info,
            "Refund dropped",
            "Nothing was sent.".to_owned(),
        ),
        OrderEvent::RefundedOutside { cents, by } => (
            ReceiptSeverity::Info,
            "Refunded elsewhere",
            format!("{} was refunded by {by}.", euros(*cents)),
        ),
        OrderEvent::ProviderOutcomeRecorded { status, reference } => (
            if *status == ExternalStatus::Accepted {
                ReceiptSeverity::Success
            } else {
                ReceiptSeverity::Warning
            },
            "Provider answered",
            format!(
                "The payment provider reported {status:?}{}.",
                reference
                    .as_ref()
                    .map_or_else(String::new, |reference| format!(" ({reference})"))
            ),
        ),
    }
}

fn receipt(committed: &CommittedEvent<OrderEvent>) -> OperationalReceipt {
    let event_ids = vec![committed.event_id];
    let status_code = committed.payload.event_type().to_owned();
    let (severity, title, body) = receipt_copy(&committed.payload);
    OperationalReceipt {
        receipt_id: ReceiptId::derive(&event_ids, &status_code),
        event_ids,
        severity,
        title: LocalizedText::new(title),
        body: LocalizedText::new(body),
        status_code,
        artifact_refs: Vec::new(),
    }
}

impl WorkflowDefinition for OrderWorkflow {
    type State = OrderState;
    type Phase = OrderPhase;
    type Obligation = OrderObligation;
    type Command = OrderCommand;
    type Event = OrderEvent;
    type Outcome = OrderOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from("order")
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &OrderPhase) -> PhaseOwnership {
        match phase {
            OrderPhase::Missing | OrderPhase::Open => PhaseOwnership::System,
            OrderPhase::AwaitingConfirmation => PhaseOwnership::User,
            OrderPhase::Sending => PhaseOwnership::External,
            OrderPhase::Refunded => PhaseOwnership::Terminal,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&OrderState>) -> ViewOf<Self> {
        let Some(state) = state else {
            return WorkflowView::new(case_ref, self.version(), OrderPhase::Missing);
        };
        let phase = phase_of(state);
        let view = WorkflowView::new(case_ref, self.version(), phase);
        match phase {
            OrderPhase::AwaitingConfirmation => view.with_blocking_interaction(
                InteractionRequirement::blocking(REFUND_CARD_KEY, InteractionKind::ConfirmCommand)
                    .with_confirms_risk(RiskClass::ExternalRegulated)
                    .with_payload(refund_payload(state)),
            ),
            OrderPhase::Refunded => view.with_outcome(OrderOutcome::Refunded),
            OrderPhase::Missing | OrderPhase::Open | OrderPhase::Sending => view,
        }
    }

    fn narratable_state(&self, state: Option<&OrderState>) -> Vec<StateField> {
        let Some(state) = state else {
            return Vec::new();
        };
        let window = if state.window_open {
            format!("open until {}", state.window_until)
        } else {
            format!("closed on {}", state.window_until)
        };
        vec![
            StateField::new("customer", serde_json::json!(state.customer)).identifying(),
            StateField::new("paid", serde_json::json!(euros(state.paid_cents))),
            StateField::new("refunded", serde_json::json!(euros(state.refunded_cents))),
            StateField::new("delivered", serde_json::json!(state.delivered.to_string())),
            StateField::new("refund_window", serde_json::json!(window)),
        ]
    }

    fn summary(&self) -> Option<String> {
        Some("Orders: what a customer paid for, and its refunds.".to_owned())
    }

    fn noun(&self) -> Option<LocalizedText> {
        Some(LocalizedText::new("order"))
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        match view.phase {
            OrderPhase::Open => vec![
                operation(
                    operations::REQUEST_REFUND,
                    "Refund an order, all or part of what was paid.",
                )
                .arguments::<RequestRefundArgs>()
                .argument("amount", |a| a.label("amount").required()),
            ],
            OrderPhase::AwaitingConfirmation => vec![
                operation(
                    operations::REFUND,
                    "Send the refund to the payment provider.",
                )
                .card_only(),
                operation(operations::DECLINE_REFUND, "Drop the refund.").card_only(),
            ],
            OrderPhase::Missing | OrderPhase::Sending | OrderPhase::Refunded => Vec::new(),
        }
    }

    fn compile_act(
        &self,
        _state: Option<&OrderState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<OrderCommand>, DomainRejection> {
        let ResolvedActKind::ApplyOperation { operation } = &act.kind else {
            return Err(reject(rejection::UNSUPPORTED));
        };
        let command = match operation.as_str() {
            operations::REQUEST_REFUND => {
                let args: RequestRefundArgs = serde_json::from_value(act.arguments.clone())
                    .map_err(|_| reject(rejection::INVALID_AMOUNT).on_argument("/amount"))?;
                if args.amount.currency != "EUR" {
                    return Err(reject(rejection::INVALID_AMOUNT).on_argument("/amount"));
                }
                OrderCommand::RequestRefund {
                    cents: args.amount.minor,
                }
            }
            operations::REFUND => OrderCommand::Refund,
            operations::DECLINE_REFUND => OrderCommand::DeclineRefund,
            _ => return Err(reject(turnframe::error::UNKNOWN_OPERATION)),
        };
        Ok(vec![command])
    }

    fn command_policy(&self, _state: Option<&OrderState>, command: &OrderCommand) -> CommandPolicy {
        match command {
            OrderCommand::Refund => CommandPolicy {
                risk: RiskClass::ExternalRegulated,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::ExternalSaga {
                    saga: "order.refund".to_owned(),
                },
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            OrderCommand::RecordProviderOutcome { .. } => CommandPolicy {
                risk: RiskClass::ExternalRegulated,
                confirmation: ConfirmationPolicy::None,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            OrderCommand::RequestRefund { .. }
            | OrderCommand::DeclineRefund
            | OrderCommand::RefundOutside { .. } => CommandPolicy::low_risk(),
        }
    }

    fn validate_command(
        &self,
        state: Option<&OrderState>,
        command: &OrderCommand,
    ) -> Result<(), DomainRejection> {
        validate(state, command)
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<OrderEvent>],
        _locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        events
            .iter()
            .filter_map(|event| match event {
                ReceiptEvent::Committed(committed) => Some(receipt(committed)),
                ReceiptEvent::Redacted(_) => None,
            })
            .collect()
    }

    fn build_interaction(
        &self,
        state: Option<&OrderState>,
        view: &ViewOf<Self>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, DomainRejection> {
        let mut spec = requirement.to_spec(view.case_ref.clone());
        if let Some(state) = state {
            // The card names what it was drawn from: a change to the order changes both.
            let drawn = canonical_digest(state).map_err(|_| reject(rejection::UNSUPPORTED))?;
            spec.payload = spec.payload.with_metadata(serde_json::json!({
                "preview_hash": drawn.as_str(),
                "bound_revision": view.case_ref.expected_revision.value(),
            }));
        }
        Ok(spec)
    }
}

impl PureWorkflow for OrderWorkflow {
    fn apply(
        &self,
        state: Option<&OrderState>,
        command: &OrderCommand,
    ) -> Result<Applied<OrderState, OrderEvent>, DomainRejection> {
        apply(state, command)
    }

    fn event_type(&self, event: &OrderEvent) -> String {
        event.event_type().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> OrderState {
        OrderState::delivered(381, "Giulia Neri", 12_900, "2026-09-18", "2026-10-18")
    }

    fn refused(state: &OrderState, command: &OrderCommand) -> String {
        validate(Some(state), command)
            .expect_err("the domain refuses it")
            .code
            .to_string()
    }

    #[test]
    fn a_refund_over_what_is_left_is_refused() {
        let command = OrderCommand::RequestRefund { cents: 129_000 };
        assert_eq!(refused(&order(), &command), rejection::EXCEEDS_PAID);
    }

    #[test]
    fn a_refund_after_the_window_closed_is_refused() {
        let closed = OrderState {
            window_open: false,
            ..order()
        };
        let command = OrderCommand::RequestRefund { cents: 12_900 };
        assert_eq!(refused(&closed, &command), rejection::WINDOW_CLOSED);
    }

    #[test]
    fn a_second_refund_while_one_is_pending_is_refused() {
        let pending = OrderState {
            pending_cents: Some(12_900),
            ..order()
        };
        let command = OrderCommand::RequestRefund { cents: 100 };
        assert_eq!(refused(&pending, &command), rejection::REFUND_PENDING);
    }

    #[test]
    fn the_card_shows_the_amount_it_was_capped_to() {
        let capped = OrderState {
            pending_cents: Some(12_900),
            refunded_cents: 3_000,
            ..order()
        };
        let payload = refund_payload(&capped);
        assert_eq!(payload.review_entries.len(), 1);
        let entry = &payload.review_entries[0];
        assert_eq!(entry.before, FieldValue::present("€129.00"));
        assert_eq!(entry.after, FieldValue::present("€99.00"));
        let whole = OrderState {
            pending_cents: Some(12_900),
            ..order()
        };
        assert!(refund_payload(&whole).review_entries.is_empty());
    }

    #[test]
    fn a_refund_sends_what_is_left_at_most() {
        let capped = OrderState {
            pending_cents: Some(12_900),
            refunded_cents: 3_000,
            ..order()
        };
        let applied = apply(Some(&capped), &OrderCommand::Refund).expect("it sends");
        assert_eq!(applied.state.sending_cents, Some(9_900));
        assert_eq!(applied.state.pending_cents, None);
    }

    #[test]
    fn money_reads_as_euros() {
        assert_eq!(euros(12_900), "€129.00");
        assert_eq!(euros(129_000), "€1,290.00");
        assert_eq!(euros(3_050), "€30.50");
    }
}
