//! The codes and the copy of what composition writes itself.

use turnframe_core::locale::LocalizedText;

/// Stable codes of the notices composition emits itself.
pub mod notice {
    /// A command did not commit, so nothing may be claimed about it.
    pub const COMMAND_FAILED: &str = "turnframe.notice.command_failed";
    /// An external effect was attempted and its result is not known yet (I15).
    pub const VERIFICATION_IN_PROGRESS: &str = "turnframe.notice.verification_in_progress";
    /// The case moved while the turn was being planned.
    pub const REVISION_CONFLICT: &str = "turnframe.notice.revision_conflict";
    /// A card the turn wanted to show could not be persisted, so it is not
    /// there and nothing refers to it (§15.5).
    pub const INTERACTION_UNAVAILABLE: &str = "turnframe.notice.interaction_unavailable";
    /// The final state could not be verified, so no follow-up was generated.
    pub const CASE_REFRESH_UNAVAILABLE: &str = "turnframe.notice.case_refresh_unavailable";
    /// A model-authored block claimed something no receipt backs and was
    /// withheld (§17.4, I16).
    /// The turn spent its resource budget, so nothing further was generated
    /// (spec §11.1). The deterministic blocks are unaffected.
    pub const BUDGET_EXHAUSTED: &str = "turnframe.notice.budget_exhausted";
    /// The user answered a card by declining the instruction it was guarding,
    /// so that instruction was not carried out (spec §13.3).
    pub const INSTRUCTION_DECLINED: &str = "turnframe.notice.instruction_declined";
}

/// Server-authored copy for everything composition writes itself.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompositionCopy {
    /// Body of the [`notice::COMMAND_FAILED`] notice.
    pub command_failed: LocalizedText,
    /// Body of the [`notice::VERIFICATION_IN_PROGRESS`] notice.
    pub verification_in_progress: LocalizedText,
    /// Body of the [`notice::REVISION_CONFLICT`] notice.
    pub revision_conflict: LocalizedText,
    /// Body of the [`notice::INTERACTION_UNAVAILABLE`] notice.
    pub interaction_unavailable: LocalizedText,
    /// Body of the [`notice::CASE_REFRESH_UNAVAILABLE`] notice.
    pub case_refresh_unavailable: LocalizedText,
    /// Text of an answer the runtime could not produce at all.
    pub answer_unsupported: LocalizedText,
    /// Text of an answer whose sources were unavailable.
    pub answer_source_unavailable: LocalizedText,
    /// Text of an answer no model wrote this time.
    pub answer_not_written: LocalizedText,
    /// Body of the [`notice::BUDGET_EXHAUSTED`] notice.
    pub budget_exhausted: LocalizedText,
    /// Body of the [`notice::INSTRUCTION_DECLINED`] notice.
    pub instruction_declined: LocalizedText,
    /// The text of an answer that ran past the configured length.
    ///
    /// Only reachable in a deployment that configured a length at all; the
    /// library ships none.
    pub answer_too_long: LocalizedText,
}

impl CompositionCopy {
    /// The built-in copy: English, with Italian.
    #[must_use]
    pub fn standard() -> Self {
        crate::copy::ServerCopy::translated(Self::english(), "it", ITALIAN)
    }

    /// English alone.
    #[must_use]
    pub fn english() -> Self {
        Self {
            command_failed: LocalizedText::new("That did not go through, so nothing was changed."),
            verification_in_progress: LocalizedText::new(
                "The request was sent and its result is not confirmed yet. It is being verified.",
            ),
            revision_conflict: LocalizedText::new(
                "This record changed while the request was being prepared, so it was not applied.",
            ),
            interaction_unavailable: LocalizedText::new(
                "The confirmation could not be prepared, so nothing is waiting for an answer.",
            ),
            case_refresh_unavailable: LocalizedText::new(
                "The latest state could not be verified, so no follow-up was prepared.",
            ),
            answer_unsupported: LocalizedText::new(
                "This question is outside what can be answered here.",
            ),
            answer_source_unavailable: LocalizedText::new(
                "The sources needed to answer this were not available.",
            ),
            answer_not_written: LocalizedText::new(
                "The answer to this could not be written just now. Asking again usually works.",
            ),
            budget_exhausted: LocalizedText::new(
                "This turn reached its limit, so nothing further was written. What was already recorded still stands.",
            ),
            instruction_declined: LocalizedText::new(
                "You said no, so that instruction was not carried out.",
            ),
            answer_too_long: LocalizedText::new(
                "The reply came back longer than this assistant is set up to send, so it was not sent.",
            ),
        }
    }
}

impl Default for CompositionCopy {
    fn default() -> Self {
        Self::standard()
    }
}

crate::copy::server_copy!(
    CompositionCopy,
    [
        command_failed,
        verification_in_progress,
        revision_conflict,
        interaction_unavailable,
        case_refresh_unavailable,
        answer_unsupported,
        answer_source_unavailable,
        answer_not_written,
        budget_exhausted,
        instruction_declined,
        answer_too_long
    ]
);

/// The built-in Italian of [`CompositionCopy`], by field.
const ITALIAN: &[(&str, &str)] = &[
    (
        "command_failed",
        "Non è andato a buon fine, quindi non è cambiato nulla.",
    ),
    (
        "verification_in_progress",
        "La richiesta è stata inviata e il risultato non è ancora confermato: è in verifica.",
    ),
    (
        "revision_conflict",
        "Questa scheda è cambiata mentre la richiesta veniva preparata, quindi non è stata applicata.",
    ),
    (
        "interaction_unavailable",
        "Non è stato possibile preparare la conferma, quindi non c'è nulla in attesa di risposta.",
    ),
    (
        "case_refresh_unavailable",
        "Non è stato possibile verificare lo stato più recente, quindi non ho preparato il passo successivo.",
    ),
    (
        "answer_unsupported",
        "A questa domanda non posso rispondere da qui.",
    ),
    (
        "answer_source_unavailable",
        "Le fonti necessarie per rispondere non erano disponibili.",
    ),
    (
        "answer_not_written",
        "Al momento non è stato possibile scrivere la risposta. Di solito basta chiedere di nuovo.",
    ),
    (
        "budget_exhausted",
        "Questa richiesta ha raggiunto il suo limite, quindi non ho scritto altro. Quello che è già stato registrato resta valido.",
    ),
    (
        "instruction_declined",
        "Hai detto di no, quindi quell'istruzione non è stata eseguita.",
    ),
    (
        "answer_too_long",
        "La risposta era più lunga di quanto questo assistente possa inviare, quindi non è stata inviata.",
    ),
];
