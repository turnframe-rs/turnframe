//! Ordered response blocks and the narration contract (spec §18).
//!
//! The assistant turn is a list of typed blocks persisted exactly as returned.
//! Model-authored blocks (answers, transitions) sit between server-authored
//! blocks (receipts, notices, interactions, artifacts) and may only cite facts
//! the server allowed. [`claim_guard::verify`] is the structural check that no
//! model-authored block claims an operational outcome without a matching
//! event-backed receipt block.

use serde::{Deserialize, Serialize};

use crate::case::CaseRef;
use crate::event::{ArtifactRef, OperationalReceipt, ReceiptSeverity};
use crate::ids::{
    AttachmentId, BlockId, ConversationId, EventId, InteractionId, OperationKey, OptionId,
    QuestionId, ReceiptId, TurnId, WorkflowKey,
};
use crate::interaction::{FieldValue, InteractionKind, InteractionStatus, InteractionView};
use crate::knowledge::Citation;
use crate::locale::{Locale, LocalizedText};
use crate::plan::AnswerBasis;
use crate::reduce::AnswerTask;

crate::ids::string_id! {
    /// Opaque token that lets a client or operator retrieve the replay record.
    ReplayToken
}

/// Severity of a server notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeSeverity {
    /// Neutral.
    Info,
    /// Needs attention.
    Warning,
    /// Something failed.
    Error,
}

/// Whether a question was answered (spec §19.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerStatus {
    /// Answered.
    Answered,
    /// The assistant needs more information.
    ClarificationRequested,
    /// Out of scope or unsupported.
    Unsupported,
    /// The required sources were unavailable.
    SourceUnavailable,
    /// An answer existed and the guard would not let it through.
    ///
    /// Distinct from [`Self::Unsupported`], and the distinction is not
    /// bookkeeping. *Unsupported* is a statement about the question: it is
    /// outside what this assistant answers. *Withheld* is a statement about one
    /// attempt at answering it: the question was in scope, an answer was
    /// written, and the claim guard refused to publish it. Reporting the second
    /// as the first tells the user their question was out of scope, which is
    /// false, and false about them rather than about the system — the one thing
    /// a guard built for honesty must not produce.
    Withheld,
    /// No model wrote an answer this time, after the retries; asking again may work.
    NotWritten,
}

/// A model-authored answer to a question (spec §18.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedAnswer {
    /// Stable block id.
    pub block_id: BlockId,
    /// The question answered, when it came from the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<QuestionId>,
    /// The text.
    pub text: String,
    /// State basis used.
    pub basis: AnswerBasis,
    /// Outcome of the answer task.
    pub status: AnswerStatus,
    /// Facts the text relies on (all must be in the allowed set).
    #[serde(default)]
    pub facts_used: Vec<NarratableFact>,
    /// Citations.
    #[serde(default)]
    pub citations: Vec<Citation>,
    /// Complete value sets the answer carries, resolved to the turn's locale.
    ///
    /// Written by the deterministic layer from a workflow's own declaration,
    /// never by a model, and rendered by the client the way a receipt is. This
    /// is how "the values this field accepts" reaches a user: as data with the
    /// workflow's labels, rather than as a sentence that could name a fourth
    /// thing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enumerations: Vec<AnsweredEnumeration>,
}

/// One complete value set as it reaches the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnsweredEnumeration {
    /// The field or concept these are the values of.
    pub subject: String,
    /// A sentence introducing them, when the workflow wrote one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
    /// Every value, in the order the workflow declared them.
    pub values: Vec<AnsweredValue>,
}

/// One value of an [`AnsweredEnumeration`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnsweredValue {
    /// Stable identifier, as the domain stores it.
    pub id: String,
    /// What a person calls it, in the turn's locale.
    pub label: String,
}

/// A short model-authored transition or acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedTransition {
    /// Stable block id.
    pub block_id: BlockId,
    /// The text.
    pub text: String,
    /// Facts the text relies on.
    #[serde(default)]
    pub facts_used: Vec<NarratableFact>,
}

/// A receipt block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptBlock {
    /// Stable block id.
    pub block_id: BlockId,
    /// The receipt.
    pub receipt: OperationalReceipt,
}

/// A server-authored notice (spec §18.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerNotice {
    /// Stable block id.
    pub block_id: BlockId,
    /// Stable code (e.g. `"turnframe.notice.nothing_submitted"`).
    pub code: String,
    /// Severity.
    pub severity: NoticeSeverity,
    /// Copy.
    pub text: LocalizedText,
}

/// An interaction block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionBlock {
    /// Stable block id.
    pub block_id: BlockId,
    /// Client-facing view.
    pub view: InteractionView,
}

/// An artifact block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactView {
    /// Stable block id.
    pub block_id: BlockId,
    /// The artifact.
    pub artifact: ArtifactRef,
}

/// One ordered block of an assistant turn (spec §18.1).
///
/// New block kinds are the obvious extension point, so downstream matches need
/// a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResponseBlock {
    /// Model-authored answer.
    Answer(GeneratedAnswer),
    /// Model-authored transition.
    Transition(GeneratedTransition),
    /// Server-authored receipt.
    Receipt(ReceiptBlock),
    /// Server-authored notice.
    Notice(ServerNotice),
    /// Persisted interaction.
    Interaction(InteractionBlock),
    /// Artifact.
    Artifact(ArtifactView),
}

impl ResponseBlock {
    /// The stable block id.
    #[must_use]
    pub fn block_id(&self) -> &BlockId {
        match self {
            Self::Answer(b) => &b.block_id,
            Self::Transition(b) => &b.block_id,
            Self::Receipt(b) => &b.block_id,
            Self::Notice(b) => &b.block_id,
            Self::Interaction(b) => &b.block_id,
            Self::Artifact(b) => &b.block_id,
        }
    }

    /// Returns `true` for blocks whose text a model wrote.
    #[must_use]
    pub fn is_model_authored(&self) -> bool {
        matches!(self, Self::Answer(_) | Self::Transition(_))
    }
}

/// The persisted assistant turn (spec §18.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantTurn {
    /// The user turn answered.
    pub turn_id: TurnId,
    /// The conversation.
    pub conversation_id: ConversationId,
    /// Ordered blocks.
    pub blocks: Vec<ResponseBlock>,
    /// Token to retrieve the replay record.
    pub replay_token: ReplayToken,
    /// The cases this turn was about, so the next turn knows what the conversation is
    /// on. Read back as a fallback for a turn with no subject of its own, never as an
    /// addition to its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<crate::case::CaseRef>,
    /// What this reply asked the user for. The next turn reads it, and only that one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expectations: Vec<Expectation>,
    /// The acts this turn did, so a correction in the next message changes what it says of
    /// one and keeps the rest. The next turn reads it, and only that one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done: Vec<DoneAct>,
    /// The next steps this reply offered, in order: the next message is read first as
    /// taking one up, and a surface may show them as choices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offers: Vec<Offer>,
}

/// A step a reply offered: taken up, it runs its operation on its record with the arguments
/// already known, and asks for the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// The record it acts on; one still to create has an empty case id.
    pub case_ref: CaseRef,
    /// The operation it runs.
    pub operation: crate::ids::OperationKey,
    /// The words the reply offered it in, in the turn's language.
    pub words: String,
    /// The arguments already known, by name.
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// An act a turn did, with the values it was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoneAct {
    /// The act, with its arguments.
    pub act: Box<crate::understanding::UnderstoodAct>,
    /// The record it changed.
    pub case_ref: CaseRef,
}

/// Something a reply asked for, so the next message can be read as its answer (§6.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Expectation {
    /// Values an act needs before it can run.
    AwaitingValue {
        /// The act, with the arguments already given.
        act: Box<crate::understanding::UnderstoodAct>,
        /// The record it applies to, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        case_ref: Option<CaseRef>,
        /// The arguments still to give.
        missing: Vec<String>,
    },
    /// An open obligation of a record, which the reply asked about.
    AwaitingObligation {
        /// The record.
        case_ref: CaseRef,
        /// The obligation, as a sentence.
        obligation: String,
    },
    /// An open obligation whose workflow named the act that answers it.
    AwaitingOperation {
        /// The record.
        case_ref: CaseRef,
        /// The obligation, as a sentence.
        obligation: String,
        /// The act that answers it.
        act: crate::flow::ObligationAct,
    },
    /// An act of an earlier turn still waiting for a record the user named that did not
    /// exist, carried until a record registered under that name completes it or the act
    /// is done another way. Never what the next message answers.
    StillWaiting {
        /// The act, with the name it gave the record.
        act: Box<crate::understanding::UnderstoodAct>,
        /// The record it applies to, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        case_ref: Option<CaseRef>,
        /// The arguments still to give.
        missing: Vec<String>,
    },
}

impl AssistantTurn {
    /// Ids of all blocks, in order.
    #[must_use]
    pub fn block_ids(&self) -> Vec<BlockId> {
        self.blocks.iter().map(|b| b.block_id().clone()).collect()
    }

    /// All receipt blocks.
    pub fn receipts(&self) -> impl Iterator<Item = &OperationalReceipt> {
        self.blocks.iter().filter_map(|b| match b {
            ResponseBlock::Receipt(r) => Some(&r.receipt),
            _ => None,
        })
    }

    /// All interaction views.
    pub fn interactions(&self) -> impl Iterator<Item = &InteractionView> {
        self.blocks.iter().filter_map(|b| match b {
            ResponseBlock::Interaction(i) => Some(&i.view),
            _ => None,
        })
    }
}

/// Voice the narrator should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToneProfile {
    /// Plain and neutral.
    #[default]
    Neutral,
    /// Friendly.
    Warm,
    /// Formal register.
    Formal,
    /// As short as possible.
    Concise,
}
/// Whether a fact is about the turn in hand or about the room it happens in.
///
/// Every record open in the account has outstanding fields, and the writer needs to know
/// them, but only the ones of the case the turn is about are what it asks next. A mark,
/// not a filter: the writing stage asks for the **first** open obligation, so
/// [`Self::ThisTurn`] sorts first and the order is load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactRelevance {
    /// About a case the turn engaged, so it is what the reply is answering.
    ///
    /// The default, because it is what every fact was taken to be before the
    /// distinction existed, including in records written then.
    #[default]
    ThisTurn,
    /// True of the account, and not what this turn is about.
    Background,
}

impl FactRelevance {
    /// Whether this is a fact about the turn in hand.
    #[must_use]
    pub const fn is_this_turn(self) -> bool {
        matches!(self, Self::ThisTurn)
    }
}

/// How far the first answer to a card got, when a second one arrives.
///
/// Four and not two, because each is a different sentence and the difference is
/// what the user came back for. "I am still working on it" must not invite a
/// third click; "I already did that" is what somebody who missed the first reply
/// wants to hear; and a card whose work failed or was closed must not be
/// described as done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnswerProgress {
    /// The commands the first answer authorized are still running.
    Running,
    /// They committed. Their events are surfaced as this turn's receipts.
    Done,
    /// They failed, so the work the card offered did not happen.
    Failed,
    /// The card was closed without doing that work — declined, dismissed,
    /// invalidated or expired.
    Closed,
}

impl AnswerProgress {
    /// How far a card in `status` got.
    ///
    /// Matched exhaustively so a status added later has to be placed here rather
    /// than falling into whichever arm is nearest.
    #[must_use]
    pub const fn of(status: InteractionStatus) -> Self {
        match status {
            InteractionStatus::Resolving => Self::Running,
            InteractionStatus::Resolved => Self::Done,
            InteractionStatus::Failed => Self::Failed,
            // A card still `Active` cannot be an already-answered one, but it is
            // no more "done" than a dismissed one, so it reads as closed rather
            // than as a claim nothing backs.
            InteractionStatus::Active
            | InteractionStatus::Declined
            | InteractionStatus::Dismissed
            | InteractionStatus::Invalidated
            | InteractionStatus::Expired => Self::Closed,
        }
    }
}

/// A fact the narrator may state (spec §18.3). Everything else is forbidden.
///
/// New fact kinds are expected, so downstream matches need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NarratableFact {
    /// Something the user can do now, in the workflow's words.
    OperationAvailable {
        /// The workflow.
        workflow: WorkflowKey,
        /// The operation.
        operation: OperationKey,
        /// What it does.
        summary: String,
    },
    /// A record a question is about: its name, where its lifecycle stands, and its
    /// outcome once complete.
    Record {
        /// The case.
        case_ref: CaseRef,
        /// The name the user knows it by, when the directory supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Its phase, in the workflow's own vocabulary.
        phase: serde_json::Value,
        /// Its outcome, when it is complete.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<serde_json::Value>,
    },
    /// A committed field value.
    StateValue {
        /// The case.
        case_ref: CaseRef,
        /// Field path.
        field: String,
        /// Value.
        value: serde_json::Value,
    },
    /// A proposed, uncommitted change.
    ProposedChange {
        /// The case.
        case_ref: CaseRef,
        /// Field path.
        field: String,
        /// Current value.
        #[serde(default, skip_serializing_if = "FieldValue::is_absent")]
        before: FieldValue,
        /// Proposed value. [`FieldValue::cleared`] is "the field is being
        /// emptied", which is not the same statement as "unchanged".
        #[serde(default, skip_serializing_if = "FieldValue::is_absent")]
        after: FieldValue,
    },
    /// An operational outcome; must be backed by a receipt block.
    OperationalOutcome {
        /// The receipt.
        receipt_id: ReceiptId,
        /// Events behind it.
        event_ids: Vec<EventId>,
        /// Status code of the receipt.
        status_code: String,
    },
    /// A card is available; must be backed by an interaction block.
    InteractionAvailable {
        /// The interaction.
        interaction_id: InteractionId,
        /// Shape.
        interaction_kind: InteractionKind,
    },
    /// The user declined an option on a card, so the server did nothing.
    ///
    /// # Why this is not [`Self::ActRefused`]
    ///
    /// The two look alike and read differently, which is the whole point. A
    /// refusal is the *server* saying no and owes the user a reason. A decline
    /// is the *user* saying no and owes them an acknowledgement — "all right, I
    /// haven't; tell me what you'd like to change" rather than "that could not
    /// be done". Folding them together would hand the narrator one word for two
    /// situations and it would write the wrong one half the time.
    ///
    /// # Why the narrator needs it at all
    ///
    /// The deterministic notice already says the instruction was declined, and
    /// it says it whether or not a model runs. What it cannot do is say it in
    /// the register of the conversation. Without this fact the narrator sees a
    /// turn with no plan, no receipts and nothing outstanding, and the only
    /// sentence that fits an empty brief is a generic offer of help — so the
    /// user reads a non-sequitur above a canned line, and it is obvious which
    /// of the two a machine wrote.
    InstructionDeclined {
        /// The case the card belonged to.
        case_ref: CaseRef,
        /// The card that was answered.
        interaction_id: InteractionId,
        /// The option that was chosen.
        option_id: OptionId,
        /// What that card asked, in the reader's locale, so the acknowledgement a
        /// decline owes names what was declined rather than offering to change something.
        #[serde(default)]
        question: String,
        /// The label of the option the user chose, in the reader's locale.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        option_label: String,
    },
    /// An act this turn prepared and did **not** run, because a card is asking
    /// the user to authorize it first.
    ///
    /// Forbidding the claim that it happened is not enough: a writer told it may not
    /// assert X, with no fact to say instead, asserts not-X. This is that fact, so the
    /// true sentence («before I do it, I am asking you») is the one it can write. It is
    /// emitted wherever the confirmation policy defers a command, since a card raised by
    /// policy moves no case and no phase or briefing knows it is there.
    ActAwaitingConfirmation {
        /// The case the act was aimed at.
        case_ref: CaseRef,
        /// The operation that is waiting.
        operation: String,
        /// What the workflow calls the thing being confirmed, when it said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
    },
    /// An act the domain refused, and why.
    ///
    /// # Why a refusal is not a claim
    ///
    /// The claim guard exists to stop a model saying something happened that
    /// did not. Saying that something did *not* happen, with the reason the
    /// server itself produced, does not cross that line — it is the honest half
    /// of the same rule. Without it an assistant forbidden from claiming
    /// success has no way to report failure either, so it writes about
    /// something else and the user concludes the write went through.
    ///
    /// That is not hypothetical: it is what happens next. A user told nothing
    /// about a refusal says "I already gave you that", the interpreter reads a
    /// transcript in which the refusal never occurred, and the complaint gets
    /// written into the field the refusal was protecting.
    ActRefused {
        /// The case the act was aimed at, when its target resolved to one.
        ///
        /// Absent when the refusal *is* the target: an act naming a shape the
        /// operation does not accept, or a card that is not on screen, never
        /// reached a case at all. The narrator is told about it anyway, because
        /// the user asked for something and did not get it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        case_ref: Option<CaseRef>,
        /// The rejection's stable code.
        code: String,
        /// The domain's own sentence, in the reader's locale, or empty when the
        /// rejection carried none.
        explanation: String,
    },
    /// An act the domain accepted, and that changed nothing.
    ///
    /// Not a refusal: nothing about it was wrong. The state it asks for is the
    /// state the case is already in, so the workflow compiled no commands —
    /// which is a legitimate answer, and the one a singleton workflow relies on
    /// when its start is proposed a second time.
    ///
    /// # Why the narrator needs it
    ///
    /// For the reason [`Self::ActRefused`] gives, and it is the same sentence:
    /// an outcome nobody is told about is read by the user as a success, and by
    /// the next turn's interpreter as a transcript in which it never happened.
    /// A refusal has been told for a long time; this one was not, and it leaves
    /// exactly the same hole — no receipt, no fact, obligations unchanged — so
    /// the writing stage does the only thing an empty brief allows. It asks for
    /// something else, or it announces the write.
    ///
    /// One turn paid for it whole. The user asked to go on with a draft; the
    /// plan proposed starting a workflow that was already open; the domain
    /// compiled nothing, the act left no trace, and the reply invented a datum
    /// on a document the user had never named.
    ActChangedNothing {
        /// The case the act reached.
        case_ref: CaseRef,
        /// The operation, as the catalogue names it. Absent for the acts that
        /// carry none — a start is the common one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<String>,
        /// The workflow's own sentence saying WHY nothing changed, in the
        /// reader's locale, or empty when it wrote none.
        ///
        /// One workflow compiles nothing for reasons that want opposite
        /// replies — the singleton whose start was proposed twice, the write
        /// that tells a field what it already says — and the name of the
        /// operation does not separate them. A writer given only the name
        /// invents, and what it reaches for is a refusal: «I cannot do that
        /// here», to a user who had asked for a correction and now has no idea
        /// what to say next.
        ///
        /// Filled from
        /// [`WorkflowDefinition::nothing_changed`](crate::flow::WorkflowDefinition::nothing_changed);
        /// empty is what every workflow said before that channel existed, and
        /// it defaults on deserialization so records written then still load.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        explanation: String,
    },
    /// An act that waits for values the user has not given: they are asked for, and
    /// nothing is written until they are.
    ValueNeeded {
        /// The case the act is aimed at, when it resolved to one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        case_ref: Option<CaseRef>,
        /// The operation.
        operation: String,
        /// The arguments asked for, by the labels people use for them.
        arguments: Vec<String>,
        /// The domain's explanation, when it refused a value it was given.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Words of the message that produced nothing to act on.
    NotUnderstood {
        /// The words, verbatim.
        words: String,
    },
    /// An act held back because another part of the message about the same record
    /// was not understood.
    ActHeld {
        /// The operation.
        operation: String,
        /// The words that were not understood.
        because: String,
    },
    /// A workflow this account cannot start yet, and the reason it declared.
    ///
    /// Shown what is unavailable, the model rightly proposes no act, so there is no
    /// refusal to carry the reason: it travels as this fact instead, like an open
    /// obligation, something that has *not* happened, known from a declaration. It widens
    /// nothing the narrator may claim: no [`ClaimClass`] covers it and no receipt backs it.
    WorkflowUnavailable {
        /// The workflow that cannot be started.
        workflow: WorkflowKey,
        /// Why, in the workflow's own words, resolved to the reader's locale.
        reason: String,
    },
    /// The card this turn answered had already been answered.
    ///
    /// # The turn this fills
    ///
    /// A second click on the same card authorizes nothing: the compare-and-set
    /// that guarantees it is the part of this worth leaving alone. But the
    /// record it comes back with used to be dropped, so from that point on
    /// nothing could tell a second click from a turn with no click at all — and
    /// a turn with no plan, no receipts and nothing outstanding still calls the
    /// stage that writes the lead-in, which does what that stage always does
    /// with an empty brief: it invents.
    ///
    /// One such turn told a user to start their invoicing configuration from the
    /// payment method. It was collected, it was on the card they were looking
    /// at, and the case's own guidance for that phase said not to start over.
    ///
    /// Skipping the stage is not the fix, because the turn would go out silent
    /// and a second click is somebody who did not understand the first answer.
    /// The fix is to give the turn something true to say.
    ///
    /// # It claims nothing this turn did
    ///
    /// The original resolution's events are surfaced beside this as receipts, so
    /// prose that says the work is done is backed by the events that did it —
    /// which is what "repeat the result without repeating the effect" means. On
    /// a resolution still running there are no events yet, and
    /// [`AnswerProgress::Running`] is the fact that keeps the prose off a claim
    /// nothing backs.
    InteractionAlreadyAnswered {
        /// The card that was clicked again.
        interaction_id: InteractionId,
        /// The option chosen the first time, when the record kept one.
        option_id: Option<OptionId>,
        /// How far the first answer got.
        progress: AnswerProgress,
    },
    /// A file the turn carried that the model was not shown.
    ///
    /// # Why the writing stage has to be told
    ///
    /// A turn's attachments reach the model as parts of the request, so a
    /// question about a document is answered from the document. When one does
    /// not get there — the application no longer holds it, the budget for one
    /// request would not take it, no provider in the chain accepts its media
    /// type — the model has the user's sentence about a file and no file.
    ///
    /// A stage asked to answer about something it cannot see answers anyway, and
    /// an answer about the wrong document is worse than an answer about none. So
    /// this is a fact and not only a notice: the notice tells the user
    /// deterministically, and the fact is what stops the prose describing a
    /// document nobody looked at.
    ///
    /// It widens nothing the narrator may claim. A file not shown is not an
    /// effect, so no [`ClaimClass`] covers it and no receipt could back it.
    AttachmentNotShown {
        /// The file, as the turn named it.
        attachment_id: AttachmentId,
        /// What the user called it, when the upload carried a name.
        filename: Option<String>,
        /// Why it was not shown, in the reader's locale.
        reason: String,
    },
    /// An obligation the case still has open.
    ///
    /// # Why this is a fact and not a licence
    ///
    /// Everything else here describes something that *happened*. This
    /// describes something that has not: a value the workflow's own projection
    /// says is still outstanding. It is on the same side of the design as the
    /// rest, because it comes from the projection rather than from the model,
    /// and the projection is a pure function of committed state.
    ///
    /// It widens nothing the narrator may claim. An obligation is not an
    /// effect, so no [`ClaimClass`] covers it and no receipt could back it; the
    /// claim guard is unmoved. What it changes is whether the assistant can ask
    /// for the next thing. A flow whose job is to collect several values could
    /// previously only acknowledge each one and wait, because the stage that
    /// writes the prose was never told what was still missing.
    ///
    /// # It must be the obligations *after* the turn
    ///
    /// The projection that framed the interpretation is the one from before
    /// the commands ran. Narrating from it asks the user for the value they
    /// have just supplied, which is worse than saying nothing. The runtime
    /// re-projects the cases a turn changed and narrates from that.
    ObligationOpen {
        /// The case, at the revision the obligation was read at.
        case_ref: CaseRef,
        /// The obligation, in the workflow's own vocabulary.
        obligation: serde_json::Value,
        /// The same thing in words a person would recognise, when the workflow
        /// says it ([`WorkflowDefinition::obligation_sentence`](crate::flow::WorkflowDefinition::obligation_sentence)).
        /// The serialized value is the domain's structure, and a stage handed only that
        /// guesses what to ask for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sentence: Option<String>,
        /// Whether this is a case the turn engaged or one that merely exists.
        ///
        /// See [`FactRelevance`]. Facts about the turn sort first, because the
        /// writing stage is told to ask for the first one.
        #[serde(default)]
        relevance: FactRelevance,
    },
    /// Retrieved knowledge.
    Knowledge {
        /// Chunk id.
        chunk_id: String,
        /// Source id.
        source_id: String,
        /// The text.
        text: String,
    },
}

/// Classes of claims the narrator must not make (spec §18.3).
///
/// Deliberately exhaustive, like [`InteractionStatus`]:
/// the list is a closed vocabulary with an [`ALL`](Self::ALL) table, and a
/// caller that forbids claims must be forced by the compiler to consider every
/// one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimClass {
    /// "Created".
    Creation,
    /// "Updated".
    Update,
    /// "Deleted".
    Deletion,
    /// "Submitted" / "sent".
    Submission,
    /// "Accepted".
    Acceptance,
    /// "Delivered".
    Delivery,
    /// "Completed".
    Completion,
    /// "You can see the card below".
    InteractionVisibility,
    /// "I will notify you".
    FutureNotification,
}

impl ClaimClass {
    /// Every class.
    pub const ALL: [Self; 9] = [
        Self::Creation,
        Self::Update,
        Self::Deletion,
        Self::Submission,
        Self::Acceptance,
        Self::Delivery,
        Self::Completion,
        Self::InteractionVisibility,
        Self::FutureNotification,
    ];
}

/// A receipt as summarized for the narrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptSummary {
    /// The receipt.
    pub receipt_id: ReceiptId,
    /// Status code.
    pub status_code: String,
    /// Severity.
    pub severity: ReceiptSeverity,
    /// Events behind it.
    pub event_ids: Vec<EventId>,
    /// The receipt's own title, in the reader's locale.
    ///
    /// # Why the narrator is shown the copy and not only the code
    ///
    /// A status code says that something happened; it does not say what. A
    /// narrator given `firm.field_set` and nothing else can only write
    /// the sentence that is true of any such event — that a datum was noted —
    /// while the receipt rendered directly beneath the prose says which field
    /// took which value. The reader gets an acknowledgement that is vaguer than
    /// the block under it, from the same turn.
    ///
    /// This does not widen what the narrator may claim. The receipt is already
    /// on screen, already passed the claim guard, and is already backed by the
    /// events it cites; letting the prose above it say the same thing is
    /// restating a claim the server has made, not making a new one.
    pub title: String,
    /// The receipt's own body, in the reader's locale. See [`Self::title`].
    pub body: String,
}

impl ReceiptSummary {
    /// Summarizes `receipt` for a reader of `locale`.
    ///
    /// The copy is resolved here rather than carried as
    /// [`crate::locale::LocalizedText`] so that what the
    /// narrator reads is exactly the string the reader will see below it. Two
    /// renderings of one receipt in one turn is the failure this avoids.
    #[must_use]
    pub fn of(receipt: &OperationalReceipt, locale: &crate::locale::Locale) -> Self {
        Self {
            receipt_id: receipt.receipt_id,
            status_code: receipt.status_code.clone(),
            severity: receipt.severity,
            event_ids: receipt.event_ids.clone(),
            title: receipt.title.resolve(locale).to_owned(),
            body: receipt.body.resolve(locale).to_owned(),
        }
    }
}

/// One option as summarized for the narrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionSummary {
    /// Option id.
    pub id: OptionId,
    /// Resolved label.
    pub label: String,
}

/// The next interaction as summarized for the narrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionSummary {
    /// The interaction.
    pub interaction_id: InteractionId,
    /// Shape.
    pub kind: InteractionKind,
    /// Whether it blocks the case.
    pub blocking: bool,
    /// What the card asks, in the reader's locale: the narrator reads the exact string
    /// the user reads beside it, as it does a receipt's. Resolved here rather than carried
    /// as [`crate::locale::LocalizedText`], for the reason [`ReceiptSummary::of`] gives.
    #[serde(default)]
    pub title: String,
    /// The card's own body, in the reader's locale. Empty when it has none.
    /// See [`Self::title`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// Options with labels resolved for the turn's locale.
    pub options: Vec<OptionSummary>,
}

/// What the runtime asks a narrator model to produce (spec §18.3). The output is
/// inserted around deterministic blocks, never trusted as the blocks themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NarrationRequest {
    /// Voice.
    pub tone: ToneProfile,
    /// Locale of the user.
    pub locale: Locale,
    /// The user's text, for acknowledgment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_text: Option<String>,
    /// Questions to answer.
    ///
    /// Empty for the transition stage, and that is the point. A transition
    /// shown a question answers it — that is what a model does with a question
    /// in its context — so the user read the same explanation twice, once in
    /// the answer block and once in the acknowledgement above it, in two
    /// different wordings. Two increasingly explicit paragraphs of prompt did
    /// not hold, because a rule the model has to remember is not a rule.
    ///
    /// The questions belong to the answer stage, which has them, writes one
    /// block each with its own basis and its own guard, and is the only stage
    /// that should be answering anything.
    pub questions: Vec<AnswerTask>,
    /// How many questions another block of this turn will answer.
    ///
    /// What the transition stage gets instead of the questions themselves: it
    /// may need to know that an answer is coming — to leave room for it, or to
    /// not ask for something that is about to be explained — and the count says
    /// that without giving it anything to answer.
    #[serde(default)]
    pub pending_questions: usize,
    /// Facts that may be stated.
    pub allowed_facts: Vec<NarratableFact>,
    /// Claims that must not be made.
    pub forbidden_claim_classes: Vec<ClaimClass>,
    /// Receipts that will surround the narration.
    pub surrounding_receipts: Vec<ReceiptSummary>,
    /// The next card, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_interaction_summary: Option<InteractionSummary>,
    /// What each workflow in view wants said, in the phase its case is in.
    ///
    /// Server-authored, one per case, and empty for a deployment whose
    /// workflows say nothing. Before this the composer had no briefing at all,
    /// so everything a domain had to say about its own voice — ask one question
    /// at a time, do not restate a summary already on screen — reached the only
    /// stage with a voice not at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guidance: Vec<CaseGuidance>,
    /// The reply that came just before this turn, when there was one.
    ///
    /// # The one input a deictic question points at
    ///
    /// The answer stage was built from the tasks, the facts, the forbidden
    /// classes and the receipts. The conversation was not in it, so a question
    /// whose subject is **the previous assistant message** reached a stage that
    /// could not see that message. Asked "in che senso?" about a sentence the
    /// assistant had just written, it had the three words, a pile of open
    /// obligations belonging to several records, and nothing about what had been
    /// asked — and told the user his own message was not specific enough.
    ///
    /// One message rather than the window, and only the assistant's. A question
    /// about the antecedent almost always means the last thing said; the current
    /// message is already here as [`Self::user_text`]; and handing this stage
    /// the whole transcript is the shape that produces prose about whatever is
    /// in front of it, which three earlier rounds were spent undoing.
    ///
    /// Absent on the first turn of a conversation, and absent from the
    /// acknowledgement stage's brief, which has no question to resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preceding_reply: Option<String>,
    /// What a person calls each case the brief mentions, as the directory labels it, so
    /// the stage that speaks to the user names a record as the user does. Server-authored,
    /// and only for cases the brief already carries: a label is what something is called,
    /// not a new subject to bring up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub case_labels: Vec<CaseLabel>,
}

/// What a person calls one case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseLabel {
    /// The case.
    pub case_ref: crate::case::CaseRef,
    /// The name the directory supplied for it, as shown to the user.
    pub label: String,
}

/// One workflow's guidance for the stage that writes, and the case it is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseGuidance {
    /// The case the guidance is about.
    pub case_ref: crate::case::CaseRef,
    /// What the workflow wants said in the phase this case is in.
    pub briefing: String,
}

/// Structural guard against unbacked operational claims (I16, spec §17.4).
pub mod claim_guard {
    use std::collections::{BTreeMap, BTreeSet};

    use serde::{Deserialize, Serialize};

    use super::{AssistantTurn, NarratableFact, ResponseBlock};
    use crate::event::ReceiptSeverity;
    use crate::ids::{BlockId, InteractionId, ReceiptId};

    /// A block claims something the turn does not back.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    #[non_exhaustive]
    pub enum ClaimViolation {
        /// A model-authored block cites an operational outcome without a
        /// matching event-backed receipt block.
        #[error(
            "block {block_id} claims outcome of receipt {receipt_id} without a backing receipt block"
        )]
        UnbackedOperationalOutcome {
            /// The offending block.
            block_id: BlockId,
            /// The receipt cited.
            receipt_id: ReceiptId,
        },
        /// A success receipt has no event ids.
        #[error("receipt block {block_id} ({receipt_id}) claims success without events")]
        ReceiptWithoutEvents {
            /// The block.
            block_id: BlockId,
            /// The receipt.
            receipt_id: ReceiptId,
        },
        /// A model-authored block refers to a card that is not in the turn.
        #[error("block {block_id} refers to interaction {interaction_id} that is not in the turn")]
        InteractionNotInTurn {
            /// The offending block.
            block_id: BlockId,
            /// The interaction cited.
            interaction_id: InteractionId,
        },
        /// Two receipt blocks of one turn claim the same receipt id.
        #[error("block {block_id} repeats receipt {receipt_id}")]
        DuplicateReceiptId {
            /// The second block carrying the id.
            block_id: BlockId,
            /// The repeated receipt.
            receipt_id: ReceiptId,
        },
    }

    /// Verifies that every operational outcome cited by a model-authored block
    /// is backed by a receipt block with a superset of its event ids and at
    /// least one event, that every success receipt has events, that no receipt
    /// id appears twice, and that every card the narration mentions is a block
    /// of the same turn.
    ///
    /// Receipt ids are unique per turn because they are derived from the events
    /// they cite ([`ReceiptId::derive`](crate::ids::ReceiptId::derive)). Merging
    /// two receipts that share an id would let a claim rest on another
    /// receipt's events.
    pub fn verify(turn: &AssistantTurn) -> Result<(), ClaimViolation> {
        let mut receipts: BTreeMap<ReceiptId, BTreeSet<_>> = BTreeMap::new();
        let mut interactions = BTreeSet::new();
        for block in &turn.blocks {
            match block {
                ResponseBlock::Receipt(r) => {
                    if r.receipt.severity == ReceiptSeverity::Success
                        && r.receipt.event_ids.is_empty()
                    {
                        return Err(ClaimViolation::ReceiptWithoutEvents {
                            block_id: r.block_id.clone(),
                            receipt_id: r.receipt.receipt_id,
                        });
                    }
                    if receipts
                        .insert(
                            r.receipt.receipt_id,
                            r.receipt.event_ids.iter().copied().collect(),
                        )
                        .is_some()
                    {
                        return Err(ClaimViolation::DuplicateReceiptId {
                            block_id: r.block_id.clone(),
                            receipt_id: r.receipt.receipt_id,
                        });
                    }
                }
                ResponseBlock::Interaction(i) => {
                    interactions.insert(i.view.id);
                }
                _ => {}
            }
        }
        for block in &turn.blocks {
            let (block_id, facts) = match block {
                ResponseBlock::Answer(a) => (&a.block_id, &a.facts_used),
                ResponseBlock::Transition(t) => (&t.block_id, &t.facts_used),
                _ => continue,
            };
            for fact in facts {
                match fact {
                    NarratableFact::OperationalOutcome {
                        receipt_id,
                        event_ids,
                        ..
                    } => {
                        let backed = receipts.get(receipt_id).is_some_and(|events| {
                            !events.is_empty() && event_ids.iter().all(|e| events.contains(e))
                        });
                        if !backed {
                            return Err(ClaimViolation::UnbackedOperationalOutcome {
                                block_id: block_id.clone(),
                                receipt_id: *receipt_id,
                            });
                        }
                    }
                    NarratableFact::InteractionAvailable { interaction_id, .. }
                        if !interactions.contains(interaction_id) =>
                    {
                        return Err(ClaimViolation::InteractionNotInTurn {
                            block_id: block_id.clone(),
                            interaction_id: *interaction_id,
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ReceiptSeverity;

    fn receipt_with_block(block: &str, id: ReceiptId, events: Vec<EventId>) -> ResponseBlock {
        ResponseBlock::Receipt(ReceiptBlock {
            block_id: BlockId::from(block),
            receipt: OperationalReceipt {
                receipt_id: id,
                event_ids: events,
                severity: ReceiptSeverity::Success,
                title: "Sent".into(),
                body: "Rebooking sent".into(),
                status_code: "trip.rebooking_sent".into(),
                artifact_refs: vec![],
            },
        })
    }

    fn receipt(id: ReceiptId, events: Vec<EventId>) -> ResponseBlock {
        receipt_with_block("r1", id, events)
    }

    fn transition(fact: NarratableFact) -> ResponseBlock {
        ResponseBlock::Transition(GeneratedTransition {
            block_id: BlockId::from("t1"),
            text: "Done".into(),
            facts_used: vec![fact],
        })
    }

    fn turn(blocks: Vec<ResponseBlock>) -> AssistantTurn {
        AssistantTurn {
            turn_id: TurnId::nil(),
            conversation_id: ConversationId::nil(),
            blocks,
            subjects: Vec::new(),
            expectations: Vec::new(),
            replay_token: ReplayToken::from("rt"),
            done: Vec::new(),
            offers: Vec::new(),
        }
    }

    #[test]
    fn outcome_claims_need_receipt_blocks() {
        let rid = ReceiptId::nil();
        let eid = EventId::nil();
        let fact = NarratableFact::OperationalOutcome {
            receipt_id: rid,
            event_ids: vec![eid],
            status_code: "trip.rebooking_sent".into(),
        };
        assert!(
            claim_guard::verify(&turn(vec![
                transition(fact.clone()),
                receipt(rid, vec![eid])
            ]))
            .is_ok()
        );
        assert!(matches!(
            claim_guard::verify(&turn(vec![transition(fact.clone())])),
            Err(claim_guard::ClaimViolation::UnbackedOperationalOutcome { .. })
        ));
        assert!(matches!(
            claim_guard::verify(&turn(vec![receipt(rid, vec![])])),
            Err(claim_guard::ClaimViolation::ReceiptWithoutEvents { .. })
        ));
        let other_event = EventId::new();
        assert!(matches!(
            claim_guard::verify(&turn(vec![
                transition(fact),
                receipt(rid, vec![other_event])
            ])),
            Err(claim_guard::ClaimViolation::UnbackedOperationalOutcome { .. })
        ));
    }

    #[test]
    fn two_receipts_may_not_share_an_id() {
        let rid = ReceiptId::nil();
        let mine = EventId::nil();
        let other = EventId::new();
        let fact = NarratableFact::OperationalOutcome {
            receipt_id: rid,
            event_ids: vec![other],
            status_code: "trip.rebooking_sent".into(),
        };
        // Without the check, the two receipts' event sets would be merged and
        // the claim would rest on the second receipt's event.
        let violation = claim_guard::verify(&turn(vec![
            transition(fact),
            receipt_with_block("r1", rid, vec![mine]),
            receipt_with_block("r2", rid, vec![other]),
        ]))
        .unwrap_err();
        assert_eq!(
            violation,
            claim_guard::ClaimViolation::DuplicateReceiptId {
                block_id: BlockId::from("r2"),
                receipt_id: rid,
            }
        );
    }

    #[test]
    fn interaction_claims_need_interaction_blocks() {
        let fact = NarratableFact::InteractionAvailable {
            interaction_id: InteractionId::nil(),
            interaction_kind: InteractionKind::ConfirmCommand,
        };
        assert!(matches!(
            claim_guard::verify(&turn(vec![transition(fact)])),
            Err(claim_guard::ClaimViolation::InteractionNotInTurn { .. })
        ));
    }

    #[test]
    fn blocks_carry_ids_and_tag() {
        let t = turn(vec![transition(NarratableFact::Knowledge {
            chunk_id: "c".into(),
            source_id: "s".into(),
            text: "t".into(),
        })]);
        assert_eq!(t.block_ids(), vec![BlockId::from("t1")]);
        let json = serde_json::to_value(&t.blocks[0]).unwrap();
        assert_eq!(json["kind"], "transition");
    }
}
