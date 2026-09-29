//! The traveler [`WorkflowDefinition`], its pure transition function and its
//! receipts.

use serde::de::DeserializeOwned;
use turnframe_core::case::CaseRef;
use turnframe_core::command::{
    AtomicityScope, ClaimMode, CommandPolicy, ConfirmationPolicy, RiskClass,
};
use turnframe_core::error::DomainRejection;
use turnframe_core::event::{OperationalReceipt, ReceiptEvent, ReceiptSeverity, RedactedEvent};
use turnframe_core::flow::{
    ConfirmationSubject, InteractionRequirement, PhaseOwnership, StartPrecondition, ViewOf,
    WorkflowDefinition, WorkflowNotice, WorkflowView,
};
use turnframe_core::ids::{OperationKey, ReceiptId, WorkflowKey, WorkflowVersion};
use turnframe_core::interaction::{
    InteractionKind, InteractionOption, InteractionPayload, OptionStyle, StoredInteractionAction,
};
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::response::NoticeSeverity;
use turnframe_core::target::{ResolvedAct, ResolvedActKind};

use crate::workflows::traveler::command::{
    CreateDraftArgs, DeclineArgs, TravelerCommand, TravelerEvent, ValueArgs, operations,
};
use crate::workflows::traveler::state::{
    DeclineReason, FieldState, TravelerObligation, TravelerOutcome, TravelerPhase, TravelerState,
    TravelerStatus,
};
use crate::workflows::{Applied, PureWorkflow};

/// Stable rejection codes of the traveler sample.
pub mod rejection {
    /// The case does not exist yet.
    pub const NOT_FOUND: &str = "traveler.not_found";
    /// The case already exists.
    pub const ALREADY_EXISTS: &str = "traveler.already_exists";
    /// The traveler can no longer be edited.
    pub const LOCKED: &str = "traveler.locked";
    /// The name is blank.
    pub const NAME_EMPTY: &str = "traveler.name_empty";
    /// The name is longer than the field allows.
    pub const NAME_TOO_LONG: &str = "traveler.name_too_long";
    /// The name holds a digit or an `@`: it is another field's value.
    pub const NOT_A_NAME: &str = "traveler.not_a_name";
    /// The address is not an address.
    pub const INVALID_EMAIL: &str = "traveler.invalid_email";
    /// The loyalty number is malformed.
    pub const INVALID_LOYALTY_NUMBER: &str = "traveler.invalid_loyalty_number";
    /// The loyalty number is already on the record, so declining it
    /// would delete a value the user gave.
    pub const LOYALTY_NUMBER_ALREADY_ANSWERED: &str = "traveler.loyalty_number_already_answered";
    /// Fields are still missing.
    pub const INCOMPLETE: &str = "traveler.incomplete";
    /// The traveler is not a draft.
    pub const NOT_A_DRAFT: &str = "traveler.not_a_draft";
    /// Only an active traveler can be archived.
    pub const NOT_ACTIVE: &str = "traveler.not_active";
    /// The traveler can no longer be deleted.
    pub const DELETE_NOT_ALLOWED: &str = "traveler.delete_not_allowed";
    /// The workflow does not compile this kind of act.
    pub const UNSUPPORTED_ACT: &str = "traveler.unsupported_act";
    /// The operation is not in the catalog.
    pub const UNKNOWN_OPERATION: &str = "traveler.unknown_operation";
    /// The arguments do not match the operation's schema.
    pub const INVALID_ARGUMENTS: &str = "traveler.invalid_arguments";
}

/// Longest accepted name, in characters.
pub const MAX_NAME_CHARS: usize = 120;

/// Fewest digits a loyalty number has after its two letters.
pub const LOYALTY_NUMBER_MIN_DIGITS: usize = 6;

/// Most digits a loyalty number has after its two letters.
pub const LOYALTY_NUMBER_MAX_DIGITS: usize = 10;

/// Returns `true` when `value` is shaped like a loyalty number: two letters, then 6 to
/// 10 digits, such as `AZ1234567`.
#[must_use]
pub fn looks_like_loyalty_number(value: &str) -> bool {
    let (letters, digits) = value.split_at(value.len().min(2));
    letters.len() == 2
        && letters.bytes().all(|b| b.is_ascii_alphabetic())
        && (LOYALTY_NUMBER_MIN_DIGITS..=LOYALTY_NUMBER_MAX_DIGITS).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Key of the blocking activation requirement.
pub const ACTIVATION_KEY: &str = "traveler.activation_confirmation";

/// Option that activates the traveler.
pub const ACTIVATE_OPTION: &str = "activate";

/// Option that keeps the traveler a draft.
pub const KEEP_DRAFT_OPTION: &str = "keep_draft";

fn reject(code: &'static str) -> DomainRejection {
    let suffix = code.strip_prefix("traveler.").unwrap_or(code);
    DomainRejection::new(code, format!("traveler.error.{suffix}"))
}

fn parse<T: DeserializeOwned>(arguments: &serde_json::Value) -> Result<T, DomainRejection> {
    serde_json::from_value(arguments.clone()).map_err(|_| reject(rejection::INVALID_ARGUMENTS))
}

/// Returns `true` when `value` looks like an address: no space, one `@` with something
/// on each side and a dot in the domain.
#[must_use]
pub fn looks_like_email(value: &str) -> bool {
    if value.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = value.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// A name the domain accepts: not blank, no digit or `@`, not too long. `argument` is
/// where the act carried it, so a refused name can be asked for again.
fn full_name_fits(value: &str, argument: &str) -> Result<(), DomainRejection> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(reject(rejection::NAME_EMPTY)
            .on_argument(argument)
            .with_explanation(
                LocalizedText::new("A traveler's name cannot be empty.").with(
                    Locale::from("it-IT"),
                    "Il nome del viaggiatore non può essere vuoto.",
                ),
            ));
    }
    if trimmed.chars().any(|c| c.is_ascii_digit() || c == '@') {
        return Err(reject(rejection::NOT_A_NAME)
            .on_argument(argument)
            .with_explanation(
            LocalizedText::new(
                "A name has no digits and no @: that looks like another of the traveler's details.",
            )
            .with(
                Locale::from("it-IT"),
                "Un nome non ha cifre né @: sembra un altro dato del viaggiatore.",
            ),
        ));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(reject(rejection::NAME_TOO_LONG)
            .with_details(serde_json::json!({ "max_chars": MAX_NAME_CHARS }))
            .on_argument(argument)
            .with_explanation(
                LocalizedText::new(format!(
                    "A traveler's name is at most {MAX_NAME_CHARS} characters."
                ))
                .with(
                    Locale::from("it-IT"),
                    format!(
                        "Il nome del viaggiatore è lungo al massimo {MAX_NAME_CHARS} caratteri."
                    ),
                ),
            ));
    }
    Ok(())
}

/// Checks a command against the current state without changing anything.
pub fn validate(
    state: Option<&TravelerState>,
    command: &TravelerCommand,
) -> Result<(), DomainRejection> {
    let Some(state) = state else {
        return match command {
            TravelerCommand::CreateDraft => Ok(()),
            TravelerCommand::CreateNamedDraft { full_name } => {
                full_name_fits(full_name, "/full_name")
            }
            _ => Err(reject(rejection::NOT_FOUND)),
        };
    };
    match command {
        TravelerCommand::CreateDraft | TravelerCommand::CreateNamedDraft { .. } => {
            Err(reject(rejection::ALREADY_EXISTS))
        }
        TravelerCommand::SetName { value } => {
            editable(state)?;
            full_name_fits(value, "/value")
        }
        TravelerCommand::ChangeEmail { value } => {
            editable(state)?;
            if looks_like_email(value.trim()) {
                Ok(())
            } else {
                Err(reject(rejection::INVALID_EMAIL)
                    .on_argument("/value")
                    .with_explanation(
                        LocalizedText::new("That is not an email address.")
                            .with(Locale::from("it-IT"), "Questo non è un indirizzo email."),
                    ))
            }
        }
        TravelerCommand::SetLoyaltyNumber { value } => {
            editable(state)?;
            let trimmed = value.trim();
            if looks_like_loyalty_number(trimmed) {
                Ok(())
            } else {
                Err(reject(rejection::INVALID_LOYALTY_NUMBER)
                    .with_details(serde_json::json!({
                        "min_digits": LOYALTY_NUMBER_MIN_DIGITS,
                        "max_digits": LOYALTY_NUMBER_MAX_DIGITS
                    }))
                    .on_argument("/value")
                    .with_explanation(
                        LocalizedText::new(
                            "A loyalty number is two letters and then 6 to 10 digits.",
                        )
                        .with(
                            Locale::from("it-IT"),
                            "Un numero fedeltà è fatto di due lettere e poi da 6 a 10 cifre.",
                        ),
                    ))
            }
        }
        TravelerCommand::DeclineLoyaltyNumber { .. } => {
            editable(state)?;
            // Declining is how an unanswered question is closed, not how an
            // answer is erased: a value already on the record stays there until
            // it is replaced by another value.
            if state.loyalty_number.is_answered() {
                return Err(reject(rejection::LOYALTY_NUMBER_ALREADY_ANSWERED));
            }
            Ok(())
        }
        TravelerCommand::Activate => {
            if state.status != TravelerStatus::Draft {
                return Err(reject(rejection::NOT_A_DRAFT));
            }
            if state.is_complete() {
                Ok(())
            } else {
                Err(reject(rejection::INCOMPLETE))
            }
        }
        TravelerCommand::Archive => {
            if state.status == TravelerStatus::Active {
                Ok(())
            } else {
                Err(reject(rejection::NOT_ACTIVE))
            }
        }
        TravelerCommand::Delete => {
            if state.status.is_editable() {
                Ok(())
            } else {
                Err(reject(rejection::DELETE_NOT_ALLOWED))
            }
        }
    }
}

fn editable(state: &TravelerState) -> Result<(), DomainRejection> {
    if state.status.is_editable() {
        Ok(())
    } else {
        Err(reject(rejection::LOCKED))
    }
}

/// Applies a command, producing the next state and the events to commit.
pub fn apply(
    state: Option<&TravelerState>,
    command: &TravelerCommand,
) -> Result<Applied<TravelerState, TravelerEvent>, DomainRejection> {
    validate(state, command)?;
    let Some(state) = state else {
        if let TravelerCommand::CreateNamedDraft { full_name } = command {
            let value = full_name.trim().to_owned();
            let named = TravelerState {
                full_name: Some(value.clone()),
                ..TravelerState::default()
            };
            return Ok(Applied::new(
                named,
                vec![
                    TravelerEvent::DraftCreated,
                    TravelerEvent::NameSet { value },
                ],
            ));
        }
        return Ok(Applied::new(
            TravelerState::default(),
            vec![TravelerEvent::DraftCreated],
        ));
    };
    let mut next = state.clone();
    let event = match command {
        TravelerCommand::CreateDraft | TravelerCommand::CreateNamedDraft { .. } => {
            return Err(reject(rejection::ALREADY_EXISTS));
        }
        TravelerCommand::SetName { value } => {
            let value = value.trim().to_owned();
            next.full_name = Some(value.clone());
            TravelerEvent::NameSet { value }
        }
        TravelerCommand::ChangeEmail { value } => {
            let value = value.trim().to_owned();
            let previous = next.email.replace(value.clone());
            TravelerEvent::EmailChanged { previous, value }
        }
        TravelerCommand::SetLoyaltyNumber { value } => {
            let value = value.trim().to_owned();
            next.loyalty_number = FieldState::answered(value.clone());
            TravelerEvent::LoyaltyNumberSet { value }
        }
        TravelerCommand::DeclineLoyaltyNumber { reason } => {
            next.loyalty_number = FieldState::declined(*reason);
            TravelerEvent::LoyaltyNumberDeclined { reason: *reason }
        }
        TravelerCommand::Activate => {
            next.status = TravelerStatus::Active;
            TravelerEvent::Activated
        }
        TravelerCommand::Archive => {
            next.status = TravelerStatus::Archived;
            TravelerEvent::Archived
        }
        TravelerCommand::Delete => {
            next.status = TravelerStatus::Deleted;
            TravelerEvent::Deleted
        }
    };
    Ok(Applied::new(next, vec![event]))
}

/// The traveler workflow: a passenger profile.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct TravelerWorkflow {
    only_while_a_trip_is_open: bool,
    with_cards: bool,
}

impl TravelerWorkflow {
    /// Builds the workflow: a traveler can be registered at any time, its fields given in
    /// text, and a card confirms its activation once every field is settled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            only_while_a_trip_is_open: false,
            with_cards: false,
        }
    }

    /// The workflow with a start precondition on another case: a traveler can only be
    /// registered while a trip is being filled in. Artificial as a rule, exact as a
    /// shape, for exercising [`StartPrecondition`].
    #[must_use]
    pub const fn only_while_a_trip_is_open() -> Self {
        Self {
            only_while_a_trip_is_open: true,
            with_cards: false,
        }
    }

    /// The same workflow with more cards: a review card for an address change and a click
    /// for deletion or archiving. For exercising the card machinery.
    #[must_use]
    pub const fn with_cards(mut self) -> Self {
        self.with_cards = true;
        self
    }

    /// The phase a state projects to.
    #[must_use]
    pub fn phase_of(state: &TravelerState) -> TravelerPhase {
        match state.status {
            TravelerStatus::Draft if state.is_complete() => TravelerPhase::AwaitingActivation,
            TravelerStatus::Draft => TravelerPhase::Collecting,
            TravelerStatus::Active => TravelerPhase::Active,
            TravelerStatus::Archived => TravelerPhase::Archived,
            TravelerStatus::Deleted => TravelerPhase::Deleted,
        }
    }

    fn compile_operation(
        operation: &OperationKey,
        arguments: &serde_json::Value,
    ) -> Result<Vec<TravelerCommand>, DomainRejection> {
        let command = match operation.as_str() {
            operations::CREATE_DRAFT => {
                let args: CreateDraftArgs = if arguments.is_null() {
                    CreateDraftArgs::default()
                } else {
                    parse(arguments)?
                };
                return Ok(vec![match args.full_name {
                    Some(full_name) => TravelerCommand::CreateNamedDraft { full_name },
                    None => TravelerCommand::CreateDraft,
                }]);
            }
            operations::SET_NAME => TravelerCommand::SetName {
                value: parse::<ValueArgs>(arguments)?.value,
            },
            operations::CHANGE_EMAIL => TravelerCommand::ChangeEmail {
                value: parse::<ValueArgs>(arguments)?.value,
            },
            operations::SET_LOYALTY_NUMBER => TravelerCommand::SetLoyaltyNumber {
                value: parse::<ValueArgs>(arguments)?.value,
            },
            operations::DECLINE_LOYALTY_NUMBER => TravelerCommand::DeclineLoyaltyNumber {
                reason: parse::<DeclineArgs>(arguments)?.reason,
            },
            operations::ACTIVATE => TravelerCommand::Activate,
            operations::ARCHIVE => TravelerCommand::Archive,
            operations::DELETE => TravelerCommand::Delete,
            // The reserved code, not this workflow's own: it says which of the
            // two mistakes this is, so the state explorer can report a
            // catalogue that offers what the compiler does not know.
            _ => return Err(reject(turnframe_core::error::UNKNOWN_OPERATION)),
        };
        Ok(vec![command])
    }
}

fn operation(key: &str, summary: &str) -> OperationSpec {
    OperationSpec::new(key)
        .summary(summary)
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
}

/// An operation writing one text field, labelled in English and Italian.
fn field(key: &str, summary: &str, label: &str, label_it: &str) -> OperationSpec {
    operation(key, summary)
        .arguments::<ValueArgs>()
        .argument("value", |a| {
            a.label(label).label_in("it-IT", label_it).required()
        })
}

/// The blocking activation card of the `AwaitingActivation` phase.
fn activation_requirement(state: &TravelerState) -> InteractionRequirement {
    let name = state.full_name.as_deref().unwrap_or("-");
    let payload = InteractionPayload::new(
        LocalizedText::new("Activate this traveler?").with("it", "Attivo questo viaggiatore?"),
    )
    .with_body(
        LocalizedText::new(format!("{name} has every required field. Activate it?")).with(
            "it",
            format!("{name} ha tutti i campi richiesti. Vuoi attivarlo?"),
        ),
    )
    .with_option(
        InteractionOption::new(
            ACTIVATE_OPTION,
            LocalizedText::new("Activate").with("it", "Attiva"),
            StoredInteractionAction::ApplyOperation {
                operation: OperationKey::from(operations::ACTIVATE),
                arguments: serde_json::Value::Null,
                freeform_argument: None,
            },
        )
        .with_style(OptionStyle::Primary),
    )
    .with_option(
        InteractionOption::new(
            KEEP_DRAFT_OPTION,
            LocalizedText::new("Keep it a draft").with("it", "Lascia in bozza"),
            StoredInteractionAction::DeclineCommands,
        )
        .with_style(OptionStyle::Danger),
    );
    InteractionRequirement::blocking(ACTIVATION_KEY, InteractionKind::ConfirmCommand)
        .with_confirms_risk(RiskClass::ReversibleLowRisk)
        .with_payload(payload)
}

/// The notice that keeps a decline visible in the view.
///
/// The obligation is gone — the assistant must stop asking — but the fact that
/// the user was asked and said no is not. Encoding the reason in the notice
/// *code* rather than only in the prose is what lets a later projection, a
/// prompt or a report branch on it without parsing a sentence.
fn declined_notice(reason: DeclineReason) -> WorkflowNotice {
    let (english, italian) = match reason {
        DeclineReason::NotApplicable => (
            "This traveler has no loyalty number. Nothing further is needed.",
            "Questo viaggiatore non ha un numero fedeltà. Non serve altro.",
        ),
        DeclineReason::Unknown => (
            "The loyalty number was not known when it was asked for. \
             It can still be supplied later.",
            "Il numero fedeltà non era noto al momento della richiesta. \
             Si può dare in seguito.",
        ),
        DeclineReason::Withheld => (
            "The traveler chose not to give a loyalty number.",
            "Il viaggiatore ha scelto di non dare il numero fedeltà.",
        ),
    };
    WorkflowNotice {
        code: format!("traveler.loyalty_number_declined.{}", reason.code()),
        severity: NoticeSeverity::Info,
        text: LocalizedText::new(english).with("it", italian),
    }
}

impl WorkflowDefinition for TravelerWorkflow {
    type State = TravelerState;
    type Phase = TravelerPhase;
    type Obligation = TravelerObligation;
    type Command = TravelerCommand;
    type Event = TravelerEvent;
    type Outcome = TravelerOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from("traveler")
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &TravelerPhase) -> PhaseOwnership {
        match phase {
            TravelerPhase::PreDraft | TravelerPhase::Collecting | TravelerPhase::Active => {
                PhaseOwnership::System
            }
            TravelerPhase::AwaitingActivation => PhaseOwnership::User,
            TravelerPhase::Archived | TravelerPhase::Deleted => PhaseOwnership::Terminal,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&TravelerState>) -> ViewOf<Self> {
        let version = self.version();
        let Some(state) = state else {
            return WorkflowView::new(case_ref, version, TravelerPhase::PreDraft);
        };
        let phase = Self::phase_of(state);
        let mut view =
            WorkflowView::new(case_ref, version, phase).with_obligations(state.open_obligations());
        // A settled-by-decline field has no obligation, so the reason is the
        // only thing left that distinguishes it from an answered one.
        if let (Some(reason), true) = (
            state.loyalty_number.decline_reason(),
            state.status.is_editable(),
        ) {
            view = view.with_notice(declined_notice(reason));
        }
        match phase {
            TravelerPhase::AwaitingActivation => {
                view.with_blocking_interaction(activation_requirement(state))
            }
            TravelerPhase::Archived => view.with_outcome(TravelerOutcome::Archived),
            TravelerPhase::Deleted => view.with_outcome(TravelerOutcome::Deleted),
            TravelerPhase::PreDraft | TravelerPhase::Collecting | TravelerPhase::Active => view,
        }
    }

    /// A card that redirects the traveler's notifications names the address.
    fn confirmation_subject(
        &self,
        _state: Option<&TravelerState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Option<ConfirmationSubject> {
        if act.operation()?.as_str() != operations::CHANGE_EMAIL {
            return None;
        }
        let email = act.arguments.get("value")?.as_str()?;
        Some(ConfirmationSubject::asking(
            LocalizedText::new(format!("Send this traveler's notifications to {email}?")).with(
                "it",
                format!("Inviare le notifiche del viaggiatore a {email}?"),
            ),
        ))
    }

    /// With [`Self::only_while_a_trip_is_open`], a start that depends on another case: the
    /// runtime declines to offer it rather than let it be proposed and refused.
    fn start_preconditions(&self) -> Vec<StartPrecondition> {
        if !self.only_while_a_trip_is_open {
            return Vec::new();
        }
        vec![StartPrecondition::new(
            "trip",
            [serde_json::json!("collecting")],
            LocalizedText::new("A traveler can only be added while a trip is open.").with(
                "it",
                "Un viaggiatore si aggiunge solo mentre un viaggio è aperto.",
            ),
        )]
    }

    fn summary(&self) -> Option<String> {
        Some(String::from(
            "Travelers: register a person and keep their details: full name, email and loyalty number.",
        ))
    }

    fn noun(&self) -> Option<LocalizedText> {
        Some(LocalizedText::new("traveler").with("it", "viaggiatore"))
    }

    /// A question one act answers is awaited as that act. The loyalty number is answered by
    /// giving it or by declining it, so its answer is routed.
    fn obligation_act(
        &self,
        _state: Option<&TravelerState>,
        obligation: &TravelerObligation,
    ) -> Option<turnframe_core::flow::ObligationAct> {
        let operation = match obligation {
            TravelerObligation::SetName => operations::SET_NAME,
            TravelerObligation::SetEmail => operations::CHANGE_EMAIL,
            TravelerObligation::SetLoyaltyNumber => return None,
        };
        Some(turnframe_core::flow::ObligationAct::new(
            operation,
            ["value"],
        ))
    }

    /// Each obligation as the question that closes it.
    fn obligation_sentence(&self, obligation: &TravelerObligation) -> Option<LocalizedText> {
        let it = Locale::from("it-IT");
        Some(match obligation {
            TravelerObligation::SetName => LocalizedText::new("What is the traveler's full name?")
                .with(it, "Qual è il nome e cognome del viaggiatore?"),
            TravelerObligation::SetEmail => {
                LocalizedText::new("Which email address reaches the traveler?")
                    .with(it, "A quale indirizzo email si scrive al viaggiatore?")
            }
            TravelerObligation::SetLoyaltyNumber => {
                LocalizedText::new("What is the traveler's loyalty number, if they have one?")
                    .with(it, "Qual è il numero fedeltà del viaggiatore, se ce l'ha?")
            }
        })
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        super::super::in_italian(Self::offered(view), ITALIAN)
    }

    fn compile_act(
        &self,
        _state: Option<&TravelerState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<TravelerCommand>, DomainRejection> {
        match &act.kind {
            // No command: the start act makes the workflow the subject of the
            // turn, and the record is opened by the operation carrying the
            // first datum. The trip workflow opens a case here instead, so the kit
            // covers both shapes a workflow may choose.
            ResolvedActKind::StartWorkflow => Ok(Vec::new()),
            ResolvedActKind::ApplyOperation { operation } => {
                Self::compile_operation(operation, &act.arguments)
            }
            _ => Err(reject(rejection::UNSUPPORTED_ACT)),
        }
    }

    /// Fields are said in text; activation is clicked. [`Self::with_cards`] adds the rest.
    fn command_policy(
        &self,
        _state: Option<&TravelerState>,
        command: &TravelerCommand,
    ) -> CommandPolicy {
        if !self.with_cards && !matches!(command, TravelerCommand::Activate) {
            return CommandPolicy::low_risk();
        }
        match command {
            TravelerCommand::Delete => CommandPolicy {
                risk: RiskClass::Destructive,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            TravelerCommand::ChangeEmail { .. } => CommandPolicy {
                risk: RiskClass::SensitiveDataChange,
                confirmation: ConfirmationPolicy::ReviewCard,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::EventReferencedParaphrase,
            },
            TravelerCommand::Activate | TravelerCommand::Archive => CommandPolicy {
                risk: RiskClass::ReversibleLowRisk,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::EventReferencedParaphrase,
            },
            TravelerCommand::CreateDraft
            | TravelerCommand::CreateNamedDraft { .. }
            | TravelerCommand::SetName { .. }
            | TravelerCommand::SetLoyaltyNumber { .. }
            | TravelerCommand::DeclineLoyaltyNumber { .. } => CommandPolicy::low_risk(),
        }
    }

    fn validate_command(
        &self,
        state: Option<&TravelerState>,
        command: &TravelerCommand,
    ) -> Result<(), DomainRejection> {
        validate(state, command)
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<TravelerEvent>],
        locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        let _ = locale;
        events
            .iter()
            .map(|event| match event {
                ReceiptEvent::Committed(committed) => {
                    let event_ids = vec![committed.event_id];
                    let status_code = committed.payload.event_type().to_owned();
                    let (title, body) = receipt_copy(&committed.payload);
                    OperationalReceipt {
                        receipt_id: ReceiptId::derive(&event_ids, &status_code),
                        event_ids,
                        severity: ReceiptSeverity::Success,
                        title,
                        body,
                        status_code,
                        artifact_refs: Vec::new(),
                    }
                }
                ReceiptEvent::Redacted(redacted) => redacted_receipt(redacted),
            })
            .collect()
    }
}

impl TravelerWorkflow {
    /// The operations a view offers, with their summaries in English.
    fn offered(view: &ViewOf<Self>) -> Vec<OperationSpec> {
        let fields = || {
            vec![
                field(
                    operations::SET_NAME,
                    "Set the traveler's full name.",
                    "full name",
                    "nome e cognome",
                )
                .example(
                    "her name is Elena Park.",
                    serde_json::json!({ "value": "Elena Park" }),
                ),
                field(
                    operations::CHANGE_EMAIL,
                    "Change the contact address.",
                    "email",
                    "email",
                ),
                field(
                    operations::SET_LOYALTY_NUMBER,
                    "Set the traveler's loyalty number.",
                    "loyalty number",
                    "numero fedeltà",
                )
                .example(
                    "the loyalty number is AZ1234567",
                    serde_json::json!({ "value": "AZ1234567" }),
                )
                .example_not_given("I'll send you the loyalty number later", ["value"]),
                operation(
                    operations::DECLINE_LOYALTY_NUMBER,
                    "Record that the traveler will not give a loyalty number, \
                     and why: it does not exist, the user does not know it, or the user will \
                     not share it.",
                )
                .arguments::<DeclineArgs>()
                .argument("reason", |a| {
                    a.label("reason")
                        .label_in("it-IT", "motivo")
                        .describe(
                            "Why no number is given: not_applicable when the traveler has \
                             none, never having joined; unknown when the user does not know \
                             it; withheld when the user will not share it.",
                        )
                        .required()
                })
                .example(
                    "she never joined the programme, she has no loyalty number",
                    serde_json::json!({ "reason": "not_applicable" }),
                ),
            ]
        };
        let delete = operation(operations::DELETE, "Remove the traveler.");
        match view.phase {
            TravelerPhase::PreDraft => vec![
                operation(
                    operations::CREATE_DRAFT,
                    "Register a new traveler: start the profile, with the full name when given.",
                )
                .target(TargetPolicy::AllowsNewCase)
                .arguments::<CreateDraftArgs>()
                .argument("full_name", |a| {
                    a.label("full name")
                        .label_in("it-IT", "nome e cognome")
                        .names_the_record()
                })
                .example(
                    "register Sofia Conti",
                    serde_json::json!({ "full_name": "Sofia Conti" }),
                ),
            ],
            TravelerPhase::Collecting => {
                let mut offered = fields();
                offered.push(delete);
                offered
            }
            TravelerPhase::AwaitingActivation => {
                let mut offered = fields();
                offered.push(operation(operations::ACTIVATE, "Make the traveler usable."));
                offered.push(delete);
                offered
            }
            TravelerPhase::Active => {
                let mut offered = fields();
                offered.push(operation(
                    operations::ARCHIVE,
                    "Keep the traveler for the record only.",
                ));
                offered.push(delete);
                offered
            }
            TravelerPhase::Archived | TravelerPhase::Deleted => Vec::new(),
        }
    }
}

/// What each operation does, in Italian.
const ITALIAN: &[(&str, &str)] = &[
    (
        operations::CREATE_DRAFT,
        "Registra un nuovo viaggiatore: ne inizia il profilo, con nome e cognome se dati.",
    ),
    (
        operations::SET_NAME,
        "Imposta nome e cognome del viaggiatore.",
    ),
    (operations::CHANGE_EMAIL, "Cambia l'indirizzo di contatto."),
    (
        operations::SET_LOYALTY_NUMBER,
        "Imposta il numero fedeltà del viaggiatore.",
    ),
    (
        operations::DECLINE_LOYALTY_NUMBER,
        "Registra che il viaggiatore non darà un numero fedeltà, e perché: non esiste, \
         l'utente non lo conosce, o non vuole darlo.",
    ),
    (operations::ACTIVATE, "Rende il viaggiatore utilizzabile."),
    (
        operations::ARCHIVE,
        "Conserva il viaggiatore solo come archivio.",
    ),
    (operations::DELETE, "Elimina il viaggiatore."),
];

/// Renders an event whose payload was erased.
///
/// This is the domain most likely to need it: a traveler record is names, loyalty
/// numbers and addresses, and every one of those values is in an event
/// payload because the receipt that announced the change quoted it. After an
/// erasure the change is still on record and the copy says so, without
/// pretending to know what the value was.
fn redacted_receipt(redacted: &RedactedEvent) -> OperationalReceipt {
    let event_ids = vec![redacted.event_id];
    let status_code = "traveler.detail_erased".to_owned();
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
            "Questa modifica resta a registro; il dettaglio di cosa e cambiato e stato cancellato.",
        ),
        status_code,
        artifact_refs: Vec::new(),
    }
}

/// Server-authored copy, in English with an Italian translation.
fn receipt_copy(event: &TravelerEvent) -> (LocalizedText, LocalizedText) {
    match event {
        TravelerEvent::DraftCreated => (
            LocalizedText::new("Draft created").with("it", "Bozza creata"),
            LocalizedText::new("A new traveler profile is open.")
                .with("it", "È aperto un nuovo profilo viaggiatore."),
        ),
        TravelerEvent::NameSet { value } => (
            LocalizedText::new("Name set").with("it", "Nome impostato"),
            LocalizedText::new(format!("The traveler's name is now \"{value}\"."))
                .with("it", format!("Il nome del viaggiatore ora è \"{value}\".")),
        ),
        TravelerEvent::EmailChanged { value, .. } => (
            LocalizedText::new("Address changed").with("it", "Indirizzo aggiornato"),
            LocalizedText::new(format!("Notifications now go to {value}."))
                .with("it", format!("Le notifiche ora vanno a {value}.")),
        ),
        TravelerEvent::LoyaltyNumberSet { value } => (
            LocalizedText::new("Loyalty number set").with("it", "Numero fedeltà impostato"),
            LocalizedText::new(format!("The loyalty number is {value}."))
                .with("it", format!("Il numero fedeltà è {value}.")),
        ),
        TravelerEvent::LoyaltyNumberDeclined { reason } => match reason {
            DeclineReason::NotApplicable => (
                LocalizedText::new("No loyalty number").with("it", "Nessun numero fedeltà"),
                LocalizedText::new("This traveler has no loyalty number.")
                    .with("it", "Questo viaggiatore non ha un numero fedeltà."),
            ),
            DeclineReason::Unknown => (
                LocalizedText::new("Loyalty number not known")
                    .with("it", "Numero fedeltà non noto"),
                LocalizedText::new("The loyalty number can be given later.")
                    .with("it", "Il numero fedeltà si può dare in seguito."),
            ),
            DeclineReason::Withheld => (
                LocalizedText::new("Loyalty number withheld").with("it", "Numero fedeltà non dato"),
                LocalizedText::new("The traveler chose not to give a loyalty number.").with(
                    "it",
                    "Il viaggiatore ha scelto di non dare il numero fedeltà.",
                ),
            ),
        },
        TravelerEvent::Activated => (
            LocalizedText::new("Traveler active").with("it", "Viaggiatore attivo"),
            LocalizedText::new("The traveler can now be put on trips.")
                .with("it", "Il viaggiatore ora si può mettere sui viaggi."),
        ),
        TravelerEvent::Archived => (
            LocalizedText::new("Traveler archived").with("it", "Viaggiatore archiviato"),
            LocalizedText::new("The traveler is kept for the record only.")
                .with("it", "Il viaggiatore resta solo a storico."),
        ),
        TravelerEvent::Deleted => (
            LocalizedText::new("Traveler deleted").with("it", "Viaggiatore eliminato"),
            LocalizedText::new("The traveler was removed.")
                .with("it", "Il viaggiatore è stato eliminato."),
        ),
    }
}

impl PureWorkflow for TravelerWorkflow {
    fn apply(
        &self,
        state: Option<&TravelerState>,
        command: &TravelerCommand,
    ) -> Result<Applied<TravelerState, TravelerEvent>, DomainRejection> {
        apply(state, command)
    }

    fn event_type(&self, event: &TravelerEvent) -> String {
        event.event_type().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_shape_is_checked() {
        assert!(looks_like_email("marta@aurora.example"));
        assert!(!looks_like_email("not-an-address"));
        assert!(!looks_like_email("@aurora.example"));
        assert!(!looks_like_email("marta@aurora"));
        assert!(!looks_like_email("marta@@aurora.example"));
        assert!(!looks_like_email("marta@.example"));
    }

    #[test]
    fn an_address_holds_no_space() {
        assert!(!looks_like_email("the email is marta@aurora.example"));
    }

    #[test]
    fn a_loyalty_number_is_two_letters_then_digits() {
        assert!(looks_like_loyalty_number("AZ1234567"));
        assert!(!looks_like_loyalty_number("1234567"));
        assert!(!looks_like_loyalty_number("AZ123"));
        assert!(!looks_like_loyalty_number("AZ12345678901"));
    }

    #[test]
    fn activation_needs_every_field() {
        let draft = TravelerState {
            full_name: Some("Marta Bianchi".into()),
            ..TravelerState::default()
        };
        let rejection = validate(Some(&draft), &TravelerCommand::Activate).unwrap_err();
        assert_eq!(rejection.code.as_str(), rejection::INCOMPLETE);

        let complete = TravelerState {
            email: Some("marta@aurora.example".into()),
            loyalty_number: FieldState::answered("AZ1234567"),
            ..draft
        };
        assert!(validate(Some(&complete), &TravelerCommand::Activate).is_ok());
    }

    #[test]
    fn a_deleted_traveler_refuses_everything() {
        let deleted = TravelerState {
            status: TravelerStatus::Deleted,
            ..TravelerState::default()
        };
        assert_eq!(
            validate(Some(&deleted), &TravelerCommand::Delete)
                .unwrap_err()
                .code
                .as_str(),
            rejection::DELETE_NOT_ALLOWED
        );
        assert_eq!(
            validate(
                Some(&deleted),
                &TravelerCommand::SetName { value: "x".into() }
            )
            .unwrap_err()
            .code
            .as_str(),
            rejection::LOCKED
        );
    }

    #[test]
    fn the_phase_follows_completeness_then_status() {
        assert_eq!(
            TravelerWorkflow::phase_of(&TravelerState::default()),
            TravelerPhase::Collecting
        );
        let complete = TravelerState {
            full_name: Some("Marta Bianchi".into()),
            email: Some("marta@aurora.example".into()),
            loyalty_number: FieldState::answered("AZ1234567"),
            status: TravelerStatus::Draft,
        };
        assert_eq!(
            TravelerWorkflow::phase_of(&complete),
            TravelerPhase::AwaitingActivation
        );
        assert!(complete.open_obligations().is_empty());
    }
}
