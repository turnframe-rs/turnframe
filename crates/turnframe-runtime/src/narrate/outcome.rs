//! What a turn did, gathered by code into the one document the acknowledgement is
//! written from, with the single thing to ask for next.

use serde::Serialize;
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::event::OperationalReceipt;
use turnframe_core::flow::ErasedWorkflowView;
use turnframe_core::interaction::Interaction;
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::response::{CaseLabel, Expectation, NarratableFact};

/// The turn as the acknowledgement may state it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct TurnOutcome {
    /// What was done, each as its receipt says it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub done: Vec<String>,
    /// What was not done, each with its reason.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_done: Vec<String>,
    /// What the user said the assistant got wrong.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disputes: Vec<String>,
    /// Workflows the turn started without writing anything yet.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub starting: Vec<String>,
    /// The one thing to ask for next.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask: Option<Ask>,
    /// The card on screen, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    /// What the user may do next, when the record needs nothing more.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub next: Vec<String>,
}

impl TurnOutcome {
    /// Whether there is nothing to say: then no acknowledgement is written at all.
    pub fn is_silent(&self) -> bool {
        self.done.is_empty()
            && self.not_done.is_empty()
            && self.disputes.is_empty()
            && self.starting.is_empty()
            && self.ask.is_none()
            && self.card.is_none()
            && self.next.is_empty()
    }

    /// Whether all the reply has to say is what it asks next.
    pub fn only_asks(&self) -> bool {
        self.ask.is_some()
            && self.done.is_empty()
            && self.not_done.is_empty()
            && self.disputes.is_empty()
            && self.starting.is_empty()
            && self.card.is_none()
            && self.next.is_empty()
    }
}

/// The one thing the reply asks for, chosen by code.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Ask {
    /// The record it is about, by its label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// What to ask for: the argument's words or the obligation's sentence.
    pub what: String,
    /// Why the value is needed again, when the domain refused the one given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub because: Option<String>,
    /// The question the reply asks when no model writes one, and the model rewords.
    pub question: String,
    /// What the next turn expects, recorded once the reply is out.
    #[serde(skip)]
    pub expectation: Option<Expectation>,
}

/// Copy for the questions code writes when no acknowledgement does.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AskCopy {
    /// A missing value; `{what}` is replaced by its label.
    pub value: LocalizedText,
    /// A value the domain refused; `{what}` is its label, `{because}` the reason.
    pub refused_value: LocalizedText,
    /// An open obligation the workflow gave no sentence; `{what}` is its name.
    pub obligation: LocalizedText,
    /// A receipt the user contested; `{what}` is the receipt as it was shown.
    pub contested: LocalizedText,
}

impl AskCopy {
    /// The built-in copy: English, with Italian.
    #[must_use]
    pub fn standard() -> Self {
        crate::copy::ServerCopy::translated(Self::english(), "it", ITALIAN)
    }

    /// English alone.
    #[must_use]
    pub fn english() -> Self {
        Self {
            value: LocalizedText::new("What should the {what} be?"),
            refused_value: LocalizedText::new("{because} What should the {what} be instead?"),
            obligation: LocalizedText::new("Still needed: {what}."),
            contested: LocalizedText::new("{what} What should it be instead?"),
        }
    }
}

impl Default for AskCopy {
    fn default() -> Self {
        Self::standard()
    }
}

crate::copy::server_copy!(AskCopy, [value, refused_value, obligation, contested]);

/// The built-in Italian of [`AskCopy`], by field.
const ITALIAN: &[(&str, &str)] = &[
    ("value", "Che cosa metto come {what}?"),
    (
        "refused_value",
        "{because} Che cosa metto come {what}, invece?",
    ),
    ("obligation", "Manca ancora: {what}."),
    ("contested", "{what} Come dovrebbe essere, invece?"),
];

fn fill(template: &LocalizedText, locale: &Locale, pairs: &[(&str, &str)]) -> String {
    let mut text = template.resolve(locale).to_owned();
    for (key, value) in pairs {
        text = text.replace(&format!("{{{key}}}"), value);
    }
    text.trim().to_owned()
}

/// What an outcome is gathered from.
pub(crate) struct Material<'a> {
    pub receipts: &'a [OperationalReceipt],
    pub facts: &'a [NarratableFact],
    pub interactions: &'a [Interaction],
    pub views: &'a [ErasedWorkflowView],
    /// The cases an act of this turn reached, in act order.
    pub touched: &'a [CaseKey],
    /// Other cases in view, asked about once the touched ones need nothing.
    pub beside: &'a [CaseKey],
    pub labels: &'a [CaseLabel],
    pub disputes: &'a [String],
    pub started: &'a [turnframe_core::ids::WorkflowKey],
    pub contested: &'a [String],
    /// What each case lets the user do next once it owes nothing, by case.
    pub next_steps: &'a [(CaseKey, Vec<LocalizedText>)],
    pub locale: &'a Locale,
    pub copy: &'a AskCopy,
}

impl Material<'_> {
    fn label(&self, case_ref: &CaseRef) -> Option<String> {
        self.labels
            .iter()
            .find(|label| label.case_ref.key() == case_ref.key())
            .map(|label| label.label.clone())
    }

    /// A receipt the user contested, else the first value an act is waiting for, else
    /// the first open obligation of a record the turn touched. A card on a record the turn
    /// reached is what comes next, so no obligation is asked beside it.
    fn ask(&self) -> Option<Ask> {
        if let Some(contested) = self.contested.first() {
            return Some(Ask {
                record: None,
                what: contested.clone(),
                because: None,
                question: fill(&self.copy.contested, self.locale, &[("what", contested)]),
                expectation: None,
            });
        }
        let waiting = self.facts.iter().find_map(|fact| match fact {
            NarratableFact::ValueNeeded {
                case_ref,
                arguments,
                reason,
                ..
            } => Some((case_ref.clone(), arguments.join(", "), reason.clone())),
            _ => None,
        });
        if let Some((case_ref, what, because)) = waiting {
            let question = match &because {
                Some(because) => fill(
                    &self.copy.refused_value,
                    self.locale,
                    &[("what", &what), ("because", because)],
                ),
                None => fill(&self.copy.value, self.locale, &[("what", &what)]),
            };
            return Some(Ask {
                record: case_ref.as_ref().and_then(|case_ref| self.label(case_ref)),
                what,
                because,
                question,
                // The waiting act is recorded as its own expectation already.
                expectation: None,
            });
        }
        let reached = |case_ref: &CaseRef| self.touched.contains(&case_ref.key());
        let card_next = self
            .interactions
            .iter()
            .any(|card| card.blocking && reached(&card.case_ref))
            || self
                .views
                .iter()
                .any(|view| view.blocking_interaction.is_some() && reached(&view.case_ref));
        if card_next {
            return None;
        }
        let (view, obligation) = self.touched.iter().chain(self.beside).find_map(|key| {
            let view = self.views.iter().find(|view| view.case_ref.key() == *key)?;
            Some((view, view.obligations.first()?))
        })?;
        // The workflow's sentence is the question; without one, its name is read out.
        let (what, question) = match &obligation.sentence {
            Some(sentence) => {
                let sentence = sentence.resolve(self.locale).to_owned();
                (sentence.clone(), sentence)
            }
            None => {
                let what = obligation_words(&obligation.value);
                let question = fill(&self.copy.obligation, self.locale, &[("what", &what)]);
                (what, question)
            }
        };
        // An obligation whose workflow names the act that answers it is awaited as that
        // act, so a bare answer completes it.
        let expectation = match &obligation.act {
            Some(act) => Expectation::AwaitingOperation {
                case_ref: view.case_ref.clone(),
                obligation: what.clone(),
                act: act.clone(),
            },
            None => Expectation::AwaitingObligation {
                case_ref: view.case_ref.clone(),
                obligation: what.clone(),
            },
        };
        Some(Ask {
            record: self.label(&view.case_ref),
            question,
            expectation: Some(expectation),
            what,
            because: None,
        })
    }

    /// Gathers the outcome.
    pub fn outcome(&self) -> TurnOutcome {
        let locale = self.locale;
        let done = self
            .receipts
            .iter()
            .map(|receipt| {
                format!(
                    "{}: {}",
                    receipt.title.resolve(locale),
                    receipt.body.resolve(locale)
                )
            })
            .collect();
        let not_done = self.facts.iter().filter_map(not_done).collect();
        let card = self.interactions.first().map(|card| {
            let options: Vec<&str> = card
                .payload
                .options
                .iter()
                .map(|option| option.label.resolve(locale))
                .collect();
            let about = self
                .touched
                .contains(&card.case_ref.key())
                .then(|| self.label(&card.case_ref))
                .flatten()
                .map(|label| format!("{label}: "))
                .unwrap_or_default();
            format!(
                "{about}{} ({})",
                card.payload.title.resolve(locale),
                options.join(" / ")
            )
        });
        let ask = self.ask();
        let next = if ask.is_none() && card.is_none() {
            self.touched
                .iter()
                .find_map(|key| {
                    self.next_steps
                        .iter()
                        .find(|(case, steps)| case == key && !steps.is_empty())
                })
                .map(|(_, steps)| {
                    steps
                        .iter()
                        .map(|step| step.resolve(locale).to_owned())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        TurnOutcome {
            done,
            not_done,
            disputes: self.disputes.to_vec(),
            starting: self.started.iter().map(ToString::to_string).collect(),
            ask,
            card,
            next,
        }
    }
}

/// An obligation's name in words: a string, or an object's one tag, with its
/// underscores read as spaces.
fn obligation_words(value: &serde_json::Value) -> String {
    let name = match value {
        serde_json::Value::String(name) => Some(name.as_str()),
        serde_json::Value::Object(map) => map.keys().next().map(String::as_str),
        _ => None,
    };
    name.map_or_else(|| value.to_string(), |name| name.replace('_', " "))
}

/// What a fact says was not done, and why, as a sentence the reply may restate.
fn not_done(fact: &NarratableFact) -> Option<String> {
    Some(match fact {
        NarratableFact::ActRefused {
            explanation, code, ..
        } => {
            if explanation.is_empty() {
                format!("Refused ({code}).")
            } else {
                explanation.clone()
            }
        }
        NarratableFact::ActChangedNothing { explanation, .. } if !explanation.is_empty() => {
            explanation.clone()
        }
        NarratableFact::ActChangedNothing { operation, .. } => format!(
            "Nothing changed: {} was already so.",
            operation.as_deref().unwrap_or("what was asked")
        ),
        NarratableFact::InstructionDeclined {
            question,
            option_label,
            ..
        } => format!("The user answered «{question}» with «{option_label}»."),
        NarratableFact::ActHeld { operation, because } => {
            format!("{operation} waits: «{because}» was not understood.")
        }
        NarratableFact::NotUnderstood { words } => format!("Not understood: «{words}»."),
        NarratableFact::WorkflowUnavailable { reason, .. } => reason.clone(),
        NarratableFact::AttachmentNotShown {
            filename, reason, ..
        } => format!(
            "The file {} was not read: {reason}",
            filename.as_deref().unwrap_or("attached")
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::ids::CaseRevision;

    fn material<'a>(
        facts: &'a [NarratableFact],
        copy: &'a AskCopy,
        locale: &'a Locale,
    ) -> Material<'a> {
        Material {
            receipts: &[],
            facts,
            interactions: &[],
            views: &[],
            touched: &[],
            beside: &[],
            labels: &[],
            disputes: &[],
            started: &[],
            contested: &[],
            next_steps: &[],
            locale,
            copy,
        }
    }

    #[test]
    fn a_record_that_needs_nothing_more_offers_its_next_steps() {
        let view = |obligations| ErasedWorkflowView {
            case_ref: CaseRef::new("sample", "s-1", CaseRevision(1)),
            workflow_version: turnframe_core::ids::WorkflowVersion::from("1"),
            phase: serde_json::json!("collecting"),
            phase_ownership: turnframe_core::flow::PhaseOwnership::User,
            obligations,
            blocking_interaction: None,
            notices: Vec::new(),
            outcome: None,
            state: Vec::new(),
        };
        let owed = turnframe_core::flow::ErasedObligation {
            id: turnframe_core::flow::ObligationId("\"a\"".to_owned()),
            value: serde_json::json!("a"),
            sentence: Some(LocalizedText::new("What is A?")),
            act: None,
        };
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        let touched = [CaseRef::new("sample", "s-1", CaseRevision(1)).key()];
        let next_steps = [(
            touched[0].clone(),
            vec![
                LocalizedText::new("Add another item."),
                LocalizedText::new("Send it."),
            ],
        )];

        let complete = [view(Vec::new())];
        let mut material = material(&[], &copy, &locale);
        material.views = &complete;
        material.touched = &touched;
        material.next_steps = &next_steps;
        let outcome = material.outcome();
        assert_eq!(outcome.ask, None);
        assert_eq!(outcome.next, vec!["Add another item.", "Send it."]);

        let open = [view(vec![owed])];
        material.views = &open;
        let outcome = material.outcome();
        assert!(outcome.ask.is_some());
        assert!(outcome.next.is_empty(), "what is owed comes first");
    }

    #[test]
    fn a_waiting_value_is_the_ask_and_code_writes_its_question() {
        let facts = [NarratableFact::ValueNeeded {
            case_ref: Some(CaseRef::new("trip", "trip-1", CaseRevision(1))),
            operation: "trip.set_name".to_owned(),
            arguments: vec!["subject".to_owned()],
            reason: None,
        }];
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        let outcome = material(&facts, &copy, &locale).outcome();
        let ask = outcome.ask.expect("an ask");
        assert_eq!(ask.what, "subject");
        assert_eq!(ask.question, "What should the subject be?");
        assert!(
            outcome.not_done.is_empty(),
            "a missing value is asked for, not reported"
        );
    }

    #[test]
    fn an_obligation_without_a_sentence_is_never_asked_by_its_code() {
        let case_ref = CaseRef::new("trip", "trip-2", CaseRevision(1));
        let view = ErasedWorkflowView {
            case_ref: case_ref.clone(),
            workflow_version: turnframe_core::ids::WorkflowVersion::from("1"),
            phase: serde_json::json!("collecting"),
            phase_ownership: turnframe_core::flow::PhaseOwnership::User,
            obligations: vec![turnframe_core::flow::ErasedObligation {
                id: turnframe_core::flow::ObligationId("\"select_traveler\"".to_owned()),
                value: serde_json::json!("select_traveler"),
                sentence: None,
                act: None,
            }],
            blocking_interaction: None,
            notices: Vec::new(),
            outcome: None,
            state: Vec::new(),
        };
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        let views = [view];
        let touched = [case_ref.key()];
        let mut material = material(&[], &copy, &locale);
        material.views = &views;
        material.touched = &touched;
        let ask = material
            .outcome()
            .ask
            .expect("the open obligation is asked for");
        assert_eq!(ask.question, "Still needed: select traveler.");
    }

    #[test]
    fn a_card_on_screen_is_what_comes_next() {
        let obligation = |case_id: &str, requirement| ErasedWorkflowView {
            case_ref: CaseRef::new("sample", case_id, CaseRevision(1)),
            workflow_version: turnframe_core::ids::WorkflowVersion::from("1"),
            phase: serde_json::json!("collecting"),
            phase_ownership: turnframe_core::flow::PhaseOwnership::User,
            obligations: vec![turnframe_core::flow::ErasedObligation {
                id: turnframe_core::flow::ObligationId("\"a\"".to_owned()),
                value: serde_json::json!("a"),
                sentence: Some(LocalizedText::new("What is A?")),
                act: None,
            }],
            blocking_interaction: requirement,
            notices: Vec::new(),
            outcome: None,
            state: Vec::new(),
        };
        let carded = obligation(
            "s-1",
            Some(turnframe_core::flow::InteractionRequirement::blocking(
                "confirm",
                turnframe_core::interaction::InteractionKind::Boolean,
            )),
        );
        let other = obligation("s-2", None);
        let touched = [carded.case_ref.key()];
        let beside = [other.case_ref.key()];
        let views = [carded, other];
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        let mut material = material(&[], &copy, &locale);
        material.views = &views;
        material.touched = &touched;
        material.beside = &beside;
        assert_eq!(material.outcome().ask, None);
    }

    #[test]
    fn nothing_to_say_is_silence() {
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        assert!(material(&[], &copy, &locale).outcome().is_silent());
    }
}
