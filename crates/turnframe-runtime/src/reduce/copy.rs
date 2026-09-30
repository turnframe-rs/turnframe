//! The codes of the runtime's own refusals and notices, and the copy it writes for them.

use super::*;

/// Rejection codes the default reducer produces.
///
/// They are stable strings, so an application can key its copy catalogue and its
/// dashboards on them.
pub mod rejection {
    /// The proposed target shape is not one the operation accepts.
    pub const TARGET_POLICY_MISMATCH: &str = "turnframe.target.policy_mismatch";
    /// The token is unknown, or belongs to another tenant.
    pub const TARGET_UNAUTHORIZED: &str = "turnframe.target.unauthorized";
    /// The case the target named no longer exists, or nothing matched.
    pub const TARGET_MISSING: &str = "turnframe.target.missing";
    /// The case moved past the revision the target was issued at.
    pub const TARGET_STALE: &str = "turnframe.target.stale";
    /// The target resolved to something this reducer does not know how to use.
    pub const TARGET_UNRESOLVED: &str = "turnframe.target.unresolved";
    /// Too many candidates matched to offer an honest choice.
    pub const TARGET_TOO_MANY_CANDIDATES: &str = "turnframe.target.too_many_candidates";
    /// The act addressed the active card and there is none.
    pub const NO_ACTIVE_INTERACTION: &str = "turnframe.interaction.none_active";
    /// The card may not be resolved from typed text (§13.2 rule 8, §15.7).
    pub const TEXT_RESOLUTION_NOT_ALLOWED: &str =
        "turnframe.interaction.text_resolution_not_allowed";
    /// The interpreted option is not one of the card's stored options.
    pub const UNKNOWN_OPTION: &str = "turnframe.interaction.unknown_option";
    /// The act named a workflow the registry does not know.
    pub const UNKNOWN_WORKFLOW: &str = "turnframe.workflow.unknown";
    /// The act named an operation this turn does not offer — or one the
    /// workflow's own `compile_act` does not recognise, which is the same
    /// sentence to a user and a defect in the deployment either way.
    ///
    /// Shared with [`turnframe_core::error::UNKNOWN_OPERATION`], which is the
    /// code a workflow returns for the second case.
    pub use turnframe_core::error::UNKNOWN_OPERATION;
    /// The act's arguments are not the shape the operation's schema demands.
    pub const INVALID_ARGUMENTS: &str = "turnframe.operation.invalid_arguments";
    /// The act named an operation only a card may run, and cites no card.
    pub const NOT_PROPOSABLE: &str = "turnframe.operation.not_proposable";
    /// The workflow the act would start declares a prerequisite this turn does
    /// not meet.
    pub const PRECONDITION_UNMET: &str = "turnframe.workflow.precondition_unmet";
    /// The act would start a workflow that resumes an open case, and the turn
    /// can see more than one.
    pub const SEVERAL_OPEN_CASES: &str = "turnframe.workflow.several_open_cases";
    /// A constraint the user placed on the turn blocks the act.
    pub const BLOCKED_BY_CONSTRAINT: &str = "turnframe.constraint.blocked";
    /// The orchestration mode is not eligible for this risk class (§11.4).
    pub const BLOCKED_BY_MODE: &str = "turnframe.policy.blocked_by_mode";
    /// The policy configuration forbids this risk class outright.
    pub const FORBIDDEN_RISK_CLASS: &str = "turnframe.policy.forbidden_risk_class";
    /// Only a qualified human other than the user may authorize this.
    pub const HUMAN_REVIEW_REQUIRED: &str = "turnframe.policy.human_review_required";
    /// The act needs a record an earlier act of the turn opens, and that act did not run.
    pub const PREREQUISITE_NOT_DONE: &str = "turnframe.operation.prerequisite_not_done";
}

/// Stable codes of the notices the default reducer emits.
pub mod notice {
    /// Nothing was submitted, because the user said not to submit.
    pub const NOTHING_SUBMITTED: &str = "turnframe.notice.nothing_submitted";
    /// Nothing was deleted, because the user said not to delete.
    pub const NOTHING_DELETED: &str = "turnframe.notice.nothing_deleted";
    /// Everything stayed a draft, because the user asked for drafts only.
    pub const DRAFT_ONLY: &str = "turnframe.notice.draft_only";
    /// No external effect was attempted, because the user forbade them.
    pub const NO_EXTERNAL_EFFECTS: &str = "turnframe.notice.no_external_effects";
    /// Part of the turn went through and part did not (§12.3 point 4).
    pub const PARTIAL_RESULT: &str = "turnframe.notice.partial_result";
    /// The domain refused an act the plan proposed.
    pub const ACT_REFUSED: &str = "turnframe.notice.act_refused";
    /// A value is asked for again, for a reason the server found.
    pub const VALUE_ASKED_AGAIN: &str = "turnframe.notice.value_asked_again";
    /// A request needed a record of a workflow, and none exists.
    pub const RECORD_NONE_YET: &str = "turnframe.notice.record_none_yet";
    /// A file the turn carried and the model was not shown.
    pub const ATTACHMENT_NOT_SHOWN: &str = "turnframe.notice.attachment_not_shown";
    /// The card this turn clicked had already been answered.
    pub const ALREADY_ANSWERED: &str = "turnframe.notice.already_answered";
    /// Part of the message produced nothing to act on.
    pub const NOT_UNDERSTOOD: &str = "turnframe.notice.not_understood";
    /// The message could not be read at all, so nothing in it ran.
    pub const MESSAGE_UNREADABLE: &str = "turnframe.notice.message_unreadable";
    /// An act was left alone because the user asked to keep what it would change.
    pub const KEPT_UNCHANGED: &str = "turnframe.notice.kept_unchanged";
}

/// Answer key of the "yes, the condition holds" option on the clarification
/// card an [`ConstraintKind::ApplyOnlyIf`] produces.
pub const CONDITION_HOLDS_ANSWER: &str = "turnframe.condition_holds";

/// Server-authored copy for everything the reducer writes itself.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NoticeCopy {
    /// Body of the [`notice::NOTHING_SUBMITTED`] notice.
    pub nothing_submitted: LocalizedText,
    /// Body of the [`notice::NOTHING_DELETED`] notice.
    pub nothing_deleted: LocalizedText,
    /// Body of the [`notice::DRAFT_ONLY`] notice.
    pub draft_only: LocalizedText,
    /// Body of the [`notice::NO_EXTERNAL_EFFECTS`] notice.
    pub no_external_effects: LocalizedText,
    /// Body of the [`notice::PARTIAL_RESULT`] notice.
    pub partial_result: LocalizedText,
    /// Title of a selection card.
    pub select_target_title: LocalizedText,
    /// Label of the "none of these" option on a selection card.
    pub select_target_cancel: LocalizedText,
    /// Title of the card an `ApplyOnlyIf` constraint raises.
    pub condition_title: LocalizedText,
    /// Label of the "yes, apply it" option on that card.
    pub condition_yes: LocalizedText,
    /// Label of the "no, leave it" option on that card.
    pub condition_no: LocalizedText,
    /// Body of the [`notice::ACT_REFUSED`] notice, for a domain rejection that carried
    /// no explanation. Deliberately vague: a workflow that wants the user told why uses
    /// [`DomainRejection::with_explanation`](turnframe_core::error::DomainRejection::with_explanation).
    /// The runtime's own refusals have their own fields below.
    pub act_refused: LocalizedText,
    /// The act aimed at a shape the operation does not accept.
    pub target_policy_mismatch: LocalizedText,
    /// The handle named a record this actor may not reach.
    pub target_unauthorized: LocalizedText,
    /// Nothing matched the record the act named.
    pub target_missing: LocalizedText,
    /// The record moved on since the handle was issued.
    pub target_stale: LocalizedText,
    /// The target resolved to something the reducer cannot use.
    pub target_unresolved: LocalizedText,
    /// Too many records matched to offer an honest choice.
    pub target_too_many_candidates: LocalizedText,
    /// The workflow the act would start already has more than one case open.
    pub several_open_cases: LocalizedText,
    /// The act addressed the card on screen and there is none.
    pub no_active_interaction: LocalizedText,
    /// The card may not be answered by typing.
    pub text_resolution_not_allowed: LocalizedText,
    /// The option named is not one the card offers.
    pub unknown_option: LocalizedText,
    /// The act named a workflow this deployment does not run.
    pub unknown_workflow: LocalizedText,
    /// The act named an operation this turn does not offer.
    pub unknown_operation: LocalizedText,
    /// The card was clicked again while its first answer is still running.
    pub already_answered_running: LocalizedText,
    /// The card was clicked again after its first answer had committed.
    pub already_answered_done: LocalizedText,
    /// The card was clicked again after its first answer had failed.
    pub already_answered_failed: LocalizedText,
    /// The card was clicked again after it had been closed without doing the
    /// work it offered.
    pub already_answered_closed: LocalizedText,
    /// The act's arguments are not what the operation takes.
    pub invalid_arguments: LocalizedText,
    /// The act named an operation only a card may run.
    pub not_proposable: LocalizedText,
    /// The orchestration mode may not carry this risk class.
    pub blocked_by_mode: LocalizedText,
    /// The configuration forbids this risk class outright.
    pub forbidden_risk_class: LocalizedText,
    /// Someone other than the user has to authorize this.
    pub human_review_required: LocalizedText,
    /// An act needed a record another act opens, and that act did not run.
    pub prerequisite_not_done: LocalizedText,
    /// Why a record the user named is asked for again: none is called that. `{workflow}`
    /// and `{named}` are replaced.
    pub record_not_found: LocalizedText,
    /// The same, when none is called that and one can be registered now.
    pub record_not_found_yet: LocalizedText,
    /// The same, when several are called that.
    pub record_not_unique: LocalizedText,
    /// A request only a record of a workflow can do, when none exists and one can be opened;
    /// `{workflow}` is what one is called.
    pub record_none_yet: LocalizedText,
    /// The same, when none can be opened now.
    pub record_none: LocalizedText,
    /// Words that produced nothing to act on; `{words}` is replaced by them.
    pub not_understood: LocalizedText,
    /// The whole message could not be read.
    pub message_unreadable: LocalizedText,
    /// Acts left alone as the user asked; `{words}` is replaced by the words asking it.
    pub kept_unchanged: LocalizedText,
}

impl NoticeCopy {
    /// The built-in copy: English, with Italian.
    #[must_use]
    pub fn standard() -> Self {
        crate::copy::ServerCopy::translated(Self::english(), "it", ITALIAN)
    }

    /// English alone.
    #[must_use]
    pub fn english() -> Self {
        Self {
            nothing_submitted: LocalizedText::new("Nothing has been submitted."),
            nothing_deleted: LocalizedText::new("Nothing has been deleted."),
            draft_only: LocalizedText::new("Everything stayed a draft."),
            no_external_effects: LocalizedText::new("No external effect was attempted."),
            partial_result: LocalizedText::new("Part of this turn still needs your input."),
            select_target_title: LocalizedText::new("Which one did you mean?"),
            select_target_cancel: LocalizedText::new("None of these"),
            condition_title: LocalizedText::new("Does that condition hold?"),
            condition_yes: LocalizedText::new("Yes, apply it"),
            condition_no: LocalizedText::new("No, leave it"),
            act_refused: LocalizedText::new("One of the things you asked for could not be done."),
            target_policy_mismatch: LocalizedText::new(
                "I could not tell which record that was meant to change.",
            ),
            target_unauthorized: LocalizedText::new(
                "That record is not one you can reach from here.",
            ),
            target_missing: LocalizedText::new("I could not find the record you meant."),
            target_stale: LocalizedText::new(
                "That record changed while we were talking, so I did not write over it.",
            ),
            target_unresolved: LocalizedText::new(
                "I could not tell which record you meant, so I wrote nothing.",
            ),
            target_too_many_candidates: LocalizedText::new(
                "Too many records match that description for me to pick one.",
            ),
            several_open_cases: LocalizedText::new(
                "You have more than one of those open already, so I did not start another. \
                 Tell me which one you mean.",
            ),
            no_active_interaction: LocalizedText::new(
                "There is no question on screen for that answer.",
            ),
            text_resolution_not_allowed: LocalizedText::new(
                "That one has to be answered with the buttons rather than in writing.",
            ),
            unknown_option: LocalizedText::new("That is not one of the choices on offer."),
            unknown_workflow: LocalizedText::new("That is not something this assistant handles."),
            unknown_operation: LocalizedText::new("That is not something I can do here."),
            already_answered_running: LocalizedText::new(
                "I am still working on that one: no need to press it again.",
            ),
            already_answered_done: LocalizedText::new("That one is already done."),
            already_answered_failed: LocalizedText::new(
                "That one did not go through, so nothing was changed by it.",
            ),
            already_answered_closed: LocalizedText::new("That one is no longer open."),
            invalid_arguments: LocalizedText::new(
                "I did not have what that needed, so I did not do it.",
            ),
            not_proposable: LocalizedText::new(
                "That one only happens when you press the button for it.",
            ),
            blocked_by_mode: LocalizedText::new("I am not set up to do that here."),
            forbidden_risk_class: LocalizedText::new("I am not allowed to do that at all."),
            human_review_required: LocalizedText::new("Someone else has to approve that one."),
            prerequisite_not_done: LocalizedText::new(
                "That depended on something else you asked for, which did not happen.",
            ),
            record_not_found: LocalizedText::new("There is no {workflow} called «{named}»."),
            record_not_found_yet: LocalizedText::new(
                "There is no {workflow} called «{named}» yet: I can register it, or you can name another.",
            ),
            record_not_unique: LocalizedText::new("More than one {workflow} is called «{named}»."),
            record_none_yet: LocalizedText::new(
                "There is no {workflow} yet to do that on: I can open one first.",
            ),
            record_none: LocalizedText::new("There is no {workflow} to do that on."),
            not_understood: LocalizedText::new("I did not understand this part: «{words}»."),
            message_unreadable: LocalizedText::new(
                "I could not make sense of that message, so I did nothing with it. \
                 Could you say it another way?",
            ),
            kept_unchanged: LocalizedText::new("Left as it is, as you asked: «{words}»."),
        }
    }

    /// What a user is told when they click a card that was already answered.
    #[must_use]
    pub fn already_answered(&self, progress: AnswerProgress) -> &LocalizedText {
        match progress {
            AnswerProgress::Running => &self.already_answered_running,
            AnswerProgress::Done => &self.already_answered_done,
            AnswerProgress::Failed => &self.already_answered_failed,
            // A variant added later reads as "no longer open", which claims the
            // least of the four.
            AnswerProgress::Closed | _ => &self.already_answered_closed,
        }
    }

    /// The notice code and body for a refusal the **runtime** decided, or
    /// `None` when the code is not one of its own.
    ///
    /// The returned code is the rejection's own, not
    /// [`notice::ACT_REFUSED`], and that is the point of the pair. Notices are
    /// deduplicated by code, so a turn in which one act named a record nobody
    /// could find and another answered a card that is not on screen used to
    /// reach the user as a single sentence covering neither. Now it reaches
    /// them as two, each keyed on a code an application can style, translate or
    /// route on.
    #[must_use]
    pub fn runtime_refusal(&self, code: &str) -> Option<(&'static str, &LocalizedText)> {
        let found = match code {
            rejection::TARGET_POLICY_MISMATCH => (
                rejection::TARGET_POLICY_MISMATCH,
                &self.target_policy_mismatch,
            ),
            rejection::TARGET_UNAUTHORIZED => {
                (rejection::TARGET_UNAUTHORIZED, &self.target_unauthorized)
            }
            rejection::TARGET_MISSING => (rejection::TARGET_MISSING, &self.target_missing),
            rejection::TARGET_STALE => (rejection::TARGET_STALE, &self.target_stale),
            rejection::TARGET_UNRESOLVED => (rejection::TARGET_UNRESOLVED, &self.target_unresolved),
            rejection::TARGET_TOO_MANY_CANDIDATES => (
                rejection::TARGET_TOO_MANY_CANDIDATES,
                &self.target_too_many_candidates,
            ),
            rejection::SEVERAL_OPEN_CASES => {
                (rejection::SEVERAL_OPEN_CASES, &self.several_open_cases)
            }
            rejection::NO_ACTIVE_INTERACTION => (
                rejection::NO_ACTIVE_INTERACTION,
                &self.no_active_interaction,
            ),
            rejection::TEXT_RESOLUTION_NOT_ALLOWED => (
                rejection::TEXT_RESOLUTION_NOT_ALLOWED,
                &self.text_resolution_not_allowed,
            ),
            rejection::UNKNOWN_OPTION => (rejection::UNKNOWN_OPTION, &self.unknown_option),
            rejection::UNKNOWN_WORKFLOW => (rejection::UNKNOWN_WORKFLOW, &self.unknown_workflow),
            rejection::UNKNOWN_OPERATION => (rejection::UNKNOWN_OPERATION, &self.unknown_operation),
            rejection::INVALID_ARGUMENTS => (rejection::INVALID_ARGUMENTS, &self.invalid_arguments),
            rejection::NOT_PROPOSABLE => (rejection::NOT_PROPOSABLE, &self.not_proposable),
            rejection::BLOCKED_BY_MODE => (rejection::BLOCKED_BY_MODE, &self.blocked_by_mode),
            rejection::FORBIDDEN_RISK_CLASS => {
                (rejection::FORBIDDEN_RISK_CLASS, &self.forbidden_risk_class)
            }
            rejection::HUMAN_REVIEW_REQUIRED => (
                rejection::HUMAN_REVIEW_REQUIRED,
                &self.human_review_required,
            ),
            rejection::PREREQUISITE_NOT_DONE => (
                rejection::PREREQUISITE_NOT_DONE,
                &self.prerequisite_not_done,
            ),
            _ => return None,
        };
        Some(found)
    }
}

impl Default for NoticeCopy {
    fn default() -> Self {
        Self::standard()
    }
}

crate::copy::server_copy!(
    NoticeCopy,
    [
        nothing_submitted,
        nothing_deleted,
        draft_only,
        no_external_effects,
        partial_result,
        select_target_title,
        select_target_cancel,
        condition_title,
        condition_yes,
        condition_no,
        act_refused,
        target_policy_mismatch,
        target_unauthorized,
        target_missing,
        target_stale,
        target_unresolved,
        target_too_many_candidates,
        several_open_cases,
        no_active_interaction,
        text_resolution_not_allowed,
        unknown_option,
        unknown_workflow,
        unknown_operation,
        already_answered_running,
        already_answered_done,
        already_answered_failed,
        already_answered_closed,
        invalid_arguments,
        not_proposable,
        blocked_by_mode,
        forbidden_risk_class,
        human_review_required,
        prerequisite_not_done,
        record_not_found,
        record_not_found_yet,
        record_not_unique,
        record_none_yet,
        record_none,
        not_understood,
        message_unreadable,
        kept_unchanged
    ]
);

/// The built-in Italian of [`NoticeCopy`], by field.
const ITALIAN: &[(&str, &str)] = &[
    (
        "record_none_yet",
        "Per farlo manca ancora un {workflow}: posso aprirne uno io.",
    ),
    ("record_none", "Per farlo manca un {workflow}."),
    ("nothing_submitted", "Non è stato inviato nulla."),
    ("nothing_deleted", "Non è stato eliminato nulla."),
    ("draft_only", "È rimasto tutto in bozza."),
    (
        "no_external_effects",
        "Non è stata tentata nessuna operazione verso l'esterno.",
    ),
    (
        "partial_result",
        "Una parte di questa richiesta aspetta ancora una tua risposta.",
    ),
    ("select_target_title", "Quale intendevi?"),
    ("select_target_cancel", "Nessuno di questi"),
    ("condition_title", "Quella condizione vale?"),
    ("condition_yes", "Sì, applicala"),
    ("condition_no", "No, lascia stare"),
    (
        "act_refused",
        "Una delle cose che hai chiesto non si è potuta fare.",
    ),
    (
        "target_policy_mismatch",
        "Non ho capito quale scheda andava modificata.",
    ),
    (
        "target_unauthorized",
        "Da qui non puoi accedere a quella scheda.",
    ),
    ("target_missing", "Non ho trovato la scheda che intendevi."),
    (
        "target_stale",
        "Quella scheda è cambiata mentre parlavamo, quindi non l'ho sovrascritta.",
    ),
    (
        "target_unresolved",
        "Non ho capito quale scheda intendevi, quindi non ho scritto nulla.",
    ),
    (
        "target_too_many_candidates",
        "Troppe schede corrispondono a quella descrizione perché possa sceglierne una.",
    ),
    (
        "several_open_cases",
        "Ce n'è già più di una aperta, quindi non ne ho aperta un'altra. Dimmi quale intendi.",
    ),
    (
        "no_active_interaction",
        "Non c'è nessuna domanda sullo schermo a cui rispondere così.",
    ),
    (
        "text_resolution_not_allowed",
        "A questa si risponde con i pulsanti, non per iscritto.",
    ),
    ("unknown_option", "Non è una delle scelte disponibili."),
    ("unknown_workflow", "Questo assistente non se ne occupa."),
    ("unknown_operation", "Qui non posso farlo."),
    (
        "already_answered_running",
        "Ci sto ancora lavorando: non serve premerlo di nuovo.",
    ),
    ("already_answered_done", "È già fatto."),
    (
        "already_answered_failed",
        "Non è andato a buon fine, quindi non ha cambiato nulla.",
    ),
    ("already_answered_closed", "Non è più aperto."),
    (
        "invalid_arguments",
        "Mancava quello che serviva, quindi non l'ho fatto.",
    ),
    (
        "not_proposable",
        "Succede solo quando premi il pulsante apposito.",
    ),
    ("blocked_by_mode", "Qui non sono configurato per farlo."),
    (
        "forbidden_risk_class",
        "Non mi è permesso farlo in nessun caso.",
    ),
    ("human_review_required", "Deve approvarlo qualcun altro."),
    (
        "prerequisite_not_done",
        "Dipendeva da un'altra cosa che hai chiesto, che non è stata fatta.",
    ),
    ("record_not_found", "Non trovo «{named}» come {workflow}."),
    (
        "record_not_found_yet",
        "Non trovo ancora «{named}» come {workflow}: posso registrarlo io, oppure puoi indicare un altro nome.",
    ),
    (
        "record_not_unique",
        "Per «{named}» come {workflow} trovo più di un risultato.",
    ),
    ("not_understood", "Non ho capito questa parte: «{words}»."),
    (
        "message_unreadable",
        "Non ho capito il messaggio, quindi non ho fatto nulla. Puoi dirlo in un altro modo?",
    ),
    (
        "kept_unchanged",
        "Lasciato com'è, come mi hai chiesto: «{words}».",
    ),
];
