//! Everything one turn's understanding may see, built by the runtime from stores and views.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use turnframe_core::flow::StateField;
use turnframe_core::ids::{OperationKey, OptionId, TargetToken, WorkflowKey};
use turnframe_core::locale::Locale;
use turnframe_core::operation::{GlossaryTerm, OperationSpec};
use turnframe_core::understanding::UnderstoodArgument;

use crate::words::Words;

/// The input of one turn's understanding.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UnderstandingInput {
    /// The user's message.
    pub message: Words,
    /// The turn's language.
    pub locale: Locale,
    /// Today, in the user's time zone: what relative dates count from.
    pub today: NaiveDate,
    /// The most recent messages before this one, oldest first.
    pub transcript: Vec<TranscriptMessage>,
    /// The workflows in view, in registration order.
    pub workflows: Vec<WorkflowBrief>,
    /// The card waiting for an answer.
    pub card: Option<OpenCard>,
    /// What the assistant asked for last turn.
    pub expectation: Option<Expectation>,
    /// Acts of earlier turns still waiting for a record the user named that did not exist.
    pub waiting: Vec<PendingAct>,
    /// Acts the last turn did, with the values they were given: a correction changes what
    /// it says of one and keeps the rest.
    pub done: Vec<PendingAct>,
    /// What the assistant reported doing last turn, so a dispute can name it.
    pub receipts: Vec<PreviousReceipt>,
    /// The records the assistant's last message was about.
    pub last_subjects: Vec<TargetToken>,
    /// How this turn is run, over the understander's own settings.
    pub settings: Option<crate::Settings>,
    /// Whether a knowledge source can answer a question about the domain in general.
    pub knowledge: bool,
    /// The next steps the last reply offered, in order: a message is read first as taking
    /// one up.
    pub offers: Vec<OfferBrief>,
}

impl UnderstandingInput {
    /// A turn saying `message` on `today`, with nothing else in view.
    #[must_use]
    pub fn new(message: &str, locale: impl Into<Locale>, today: NaiveDate) -> Self {
        Self {
            message: Words::split(message),
            locale: locale.into(),
            today,
            transcript: Vec::new(),
            workflows: Vec::new(),
            card: None,
            expectation: None,
            waiting: Vec::new(),
            done: Vec::new(),
            receipts: Vec::new(),
            last_subjects: Vec::new(),
            settings: None,
            knowledge: true,
            offers: Vec::new(),
        }
    }

    /// Adds a next step the last reply offered.
    #[must_use]
    pub fn with_offer(mut self, offer: OfferBrief) -> Self {
        self.offers.push(offer);
        self
    }

    /// Says whether a knowledge source can answer a question about the domain in general;
    /// without one, no question is read as asking it.
    #[must_use]
    pub const fn with_knowledge(mut self, knowledge: bool) -> Self {
        self.knowledge = knowledge;
        self
    }

    /// Runs this turn under `settings` instead of the understander's.
    #[must_use]
    pub const fn with_settings(mut self, settings: crate::Settings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Adds a workflow.
    #[must_use]
    pub fn with_workflow(mut self, workflow: WorkflowBrief) -> Self {
        self.workflows.push(workflow);
        self
    }

    /// Adds an earlier message.
    #[must_use]
    pub fn with_earlier(mut self, speaker: Speaker, text: &str) -> Self {
        self.transcript.push(TranscriptMessage {
            speaker,
            words: Words::split(text),
        });
        self
    }

    /// Sets the card on screen.
    #[must_use]
    pub fn with_card(mut self, card: OpenCard) -> Self {
        self.card = Some(card);
        self
    }

    /// Sets what the assistant asked for.
    #[must_use]
    pub fn with_expectation(mut self, expectation: Expectation) -> Self {
        self.expectation = Some(expectation);
        self
    }

    /// Adds an act of an earlier turn still waiting for a record the user named.
    #[must_use]
    pub fn with_waiting(mut self, act: PendingAct) -> Self {
        self.waiting.push(act);
        self
    }

    /// Adds an act the last turn did.
    #[must_use]
    pub fn with_done(mut self, act: PendingAct) -> Self {
        self.done.push(act);
        self
    }

    /// Adds a receipt of the last turn.
    #[must_use]
    pub fn with_receipt(mut self, receipt: PreviousReceipt) -> Self {
        self.receipts.push(receipt);
        self
    }

    /// Adds a record the assistant's last message was about.
    #[must_use]
    pub fn with_last_subject(mut self, record: TargetToken) -> Self {
        if !self.last_subjects.contains(&record) {
            self.last_subjects.push(record);
        }
        self
    }

    /// The workflow with this key.
    #[must_use]
    pub fn workflow(&self, key: &WorkflowKey) -> Option<&WorkflowBrief> {
        self.workflows.iter().find(|workflow| &workflow.key == key)
    }

    /// The operation with this key, and its workflow.
    #[must_use]
    pub fn operation(&self, key: &OperationKey) -> Option<(&WorkflowBrief, &OperationSpec)> {
        self.workflows.iter().find_map(|workflow| {
            workflow
                .operations
                .iter()
                .find(|spec| &spec.key == key)
                .map(|spec| (workflow, spec))
        })
    }

    /// The record with this token, and its workflow.
    #[must_use]
    pub fn record(&self, token: &TargetToken) -> Option<(&WorkflowBrief, &RecordBrief)> {
        self.workflows.iter().find_map(|workflow| {
            workflow
                .records
                .iter()
                .find(|record| &record.token == token)
                .map(|record| (workflow, record))
        })
    }
}

/// Who wrote an earlier message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    /// The user.
    User,
    /// The assistant.
    Assistant,
}

/// An earlier message, split into words so a value from it can be pointed at.
#[derive(Debug, Clone)]
pub struct TranscriptMessage {
    /// Who wrote it.
    pub speaker: Speaker,
    /// What it said.
    pub words: Words,
}

/// A workflow as understanding sees it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WorkflowBrief {
    /// Its key.
    pub key: WorkflowKey,
    /// One line on what it is for.
    pub summary: Option<String>,
    /// What its words mean.
    pub glossary: Vec<GlossaryTerm>,
    /// Whether a new case of it may be started.
    pub startable: bool,
    /// Every operation offered on a record in view or on a new case, each once.
    pub operations: Vec<OperationSpec>,
    /// The records in view.
    pub records: Vec<RecordBrief>,
    /// The operations offered on a record that does not exist yet.
    pub new_case: Vec<OperationKey>,
    /// What a question about it may be about: its enumerations and stated fields.
    pub subjects: Vec<String>,
}

impl WorkflowBrief {
    /// A workflow with nothing in view.
    #[must_use]
    pub fn new(key: impl Into<WorkflowKey>) -> Self {
        Self {
            key: key.into(),
            summary: None,
            glossary: Vec::new(),
            startable: false,
            operations: Vec::new(),
            records: Vec::new(),
            new_case: Vec::new(),
            subjects: Vec::new(),
        }
    }

    /// Sets its summary.
    #[must_use]
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// Adds a glossary term.
    #[must_use]
    pub fn term(mut self, term: GlossaryTerm) -> Self {
        self.glossary.push(term);
        self
    }

    /// Lets a new case be started.
    #[must_use]
    pub const fn startable(mut self) -> Self {
        self.startable = true;
        self
    }

    /// Adds an operation, stamped with this workflow.
    #[must_use]
    pub fn operation(mut self, mut spec: OperationSpec) -> Self {
        spec.workflow = self.key.clone();
        self.operations.push(spec);
        self
    }

    /// Adds a record.
    #[must_use]
    pub fn record(mut self, record: RecordBrief) -> Self {
        self.records.push(record);
        self
    }

    /// Offers an operation on a new case.
    #[must_use]
    pub fn on_new_case(mut self, operation: impl Into<OperationKey>) -> Self {
        self.new_case.push(operation.into());
        self
    }

    /// Adds a subject questions may be about.
    #[must_use]
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subjects.push(subject.into());
        self
    }

    /// The operation with this key.
    #[must_use]
    pub fn spec(&self, key: &OperationKey) -> Option<&OperationSpec> {
        self.operations.iter().find(|spec| &spec.key == key)
    }
}

/// A record in view.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RecordBrief {
    /// The token it is named by.
    pub token: TargetToken,
    /// Server-authored label.
    pub label: String,
    /// Its phase, as the workflow names it.
    pub phase: String,
    /// What it holds, as the workflow says it may be stated.
    pub fields: Vec<StateField>,
    /// What it still needs, as sentences.
    pub obligations: Vec<String>,
    /// The workflow's guidance for a turn about a record in this phase.
    pub briefing: Option<String>,
    /// The operations offered on it now.
    pub operations: Vec<OperationKey>,
}

impl RecordBrief {
    /// A record with its token, label and phase.
    #[must_use]
    pub fn new(
        token: impl Into<TargetToken>,
        label: impl Into<String>,
        phase: impl Into<String>,
    ) -> Self {
        Self {
            token: token.into(),
            label: label.into(),
            phase: phase.into(),
            fields: Vec::new(),
            obligations: Vec::new(),
            briefing: None,
            operations: Vec::new(),
        }
    }

    /// Adds a field.
    #[must_use]
    pub fn field(mut self, field: StateField) -> Self {
        self.fields.push(field);
        self
    }

    /// Adds an open obligation.
    #[must_use]
    pub fn obligation(mut self, sentence: impl Into<String>) -> Self {
        self.obligations.push(sentence.into());
        self
    }

    /// Sets the phase guidance.
    #[must_use]
    pub fn briefing(mut self, briefing: impl Into<String>) -> Self {
        self.briefing = Some(briefing.into());
        self
    }

    /// Offers operations on it.
    #[must_use]
    pub fn offering<I, K>(mut self, operations: I) -> Self
    where
        I: IntoIterator<Item = K>,
        K: Into<OperationKey>,
    {
        self.operations
            .extend(operations.into_iter().map(Into::into));
        self
    }

    /// Whether `operation` is offered on it.
    #[must_use]
    pub fn offers(&self, operation: &OperationKey) -> bool {
        self.operations.contains(operation)
    }
}

/// The card on screen, as its question and option labels.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OpenCard {
    /// The workflow it belongs to.
    pub workflow: WorkflowKey,
    /// The record it is about.
    pub record: Option<TargetToken>,
    /// Its question.
    pub question: String,
    /// Its options.
    pub options: Vec<CardOption>,
    /// Whether typed text may answer it at all.
    pub accepts_typed_answer: bool,
}

impl OpenCard {
    /// A card asking `question` about a record of `workflow`.
    #[must_use]
    pub fn new(workflow: impl Into<WorkflowKey>, question: impl Into<String>) -> Self {
        Self {
            workflow: workflow.into(),
            record: None,
            question: question.into(),
            options: Vec::new(),
            accepts_typed_answer: true,
        }
    }

    /// Sets the record it is about.
    #[must_use]
    pub fn about(mut self, record: impl Into<TargetToken>) -> Self {
        self.record = Some(record.into());
        self
    }

    /// Adds an option.
    #[must_use]
    pub fn option(mut self, id: impl Into<OptionId>, label: impl Into<String>) -> Self {
        self.options.push(CardOption {
            id: id.into(),
            label: label.into(),
        });
        self
    }

    /// Refuses typed answers: only a click resolves it.
    #[must_use]
    pub const fn click_only(mut self) -> Self {
        self.accepts_typed_answer = false;
        self
    }
}

/// One option of a card.
#[derive(Debug, Clone)]
pub struct CardOption {
    /// Its identifier.
    pub id: OptionId,
    /// Its label.
    pub label: String,
}

/// What the assistant asked for last turn.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Expectation {
    /// Values an act needs before it can run.
    Values(PendingAct),
    /// An open obligation of a record.
    Obligation {
        /// The record.
        record: TargetToken,
        /// The obligation, as a sentence.
        sentence: String,
    },
}

impl Expectation {
    /// The record the assistant asked about, when it named one.
    #[must_use]
    pub const fn record(&self) -> Option<&TargetToken> {
        match self {
            Self::Values(pending) => pending.record.as_ref(),
            Self::Obligation { record, .. } => Some(record),
        }
    }
}

/// A next step the last reply offered: taken up, it is this act, on its record, with the
/// values it already has; the words are those the reply offered it in.
#[derive(Debug, Clone)]
pub struct OfferBrief {
    /// The words the reply offered it in.
    pub words: String,
    /// The act it runs.
    pub act: PendingAct,
}

impl OfferBrief {
    /// An offer of `act`, made in `words`.
    #[must_use]
    pub fn new(words: impl Into<String>, act: PendingAct) -> Self {
        Self {
            words: words.into(),
            act,
        }
    }
}

/// An act of an earlier turn with the values it was given: one waiting for more, or one done.
#[derive(Debug, Clone)]
pub struct PendingAct {
    /// The operation.
    pub operation: OperationKey,
    /// The record it applies to.
    pub record: Option<TargetToken>,
    /// The arguments already given.
    pub given: BTreeMap<String, UnderstoodArgument>,
    /// The arguments asked for.
    pub missing: Vec<String>,
}

/// A change the assistant reported last turn.
#[derive(Debug, Clone)]
pub struct PreviousReceipt {
    /// The key it is shown under, such as `r1`.
    pub key: String,
    /// What it said, as shown.
    pub text: String,
}

impl PreviousReceipt {
    /// A receipt shown under `key`.
    #[must_use]
    pub fn new(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
        }
    }
}
