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
    /// The question a reply ends on when nothing is asked, no card is on screen and nothing
    /// comes next: no reply leaves the user without a way forward.
    #[serde(skip)]
    pub closing: Option<String>,
    /// The next steps as the operations they run, recorded on the turn as its offers.
    #[serde(skip)]
    pub offers: Vec<turnframe_core::response::Offer>,
    /// Where the records of questions no fact answered stand.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub standing: Vec<Standing>,
}

/// Where a record stands: what it holds and what it still needs, as its workflow states them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Standing {
    /// The record, by its label.
    pub record: String,
    /// What it holds, each as «field: value».
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<String>,
    /// What it still needs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
}

impl Standing {
    /// What it holds, as one line code may say; what it needs is the reply's ask.
    pub fn line(&self) -> Option<String> {
        (!self.holds.is_empty()).then(|| format!("{}: {}.", self.record, self.holds.join(", ")))
    }
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
    /// Whether the last reply asked the same of the same record, and nothing refused it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub again: bool,
    /// The question the reply asks when no model writes one, and the model rewords.
    pub question: String,
    /// Whether it is about a record the turn did not reach, so the question names it.
    #[serde(skip)]
    pub elsewhere: bool,
    /// What the next turn expects, recorded once the reply is out.
    #[serde(skip)]
    pub expectation: Option<Expectation>,
    /// Its record and the obligation or operation it waits on, to match the last reply's.
    #[serde(skip)]
    pub about: Option<(CaseKey, String)>,
}

/// What an expectation asked, in the terms of [`Ask::about`].
fn asked(expectation: &Expectation) -> Option<(CaseKey, String)> {
    match expectation {
        Expectation::AwaitingObligation {
            case_ref,
            obligation,
        }
        | Expectation::AwaitingOperation {
            case_ref,
            obligation,
            ..
        } => Some((case_ref.key(), obligation.clone())),
        Expectation::AwaitingValue {
            act,
            case_ref: Some(case_ref),
            ..
        } => Some((case_ref.key(), act.operation()?.as_str().to_owned())),
        _ => None,
    }
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
    /// A question about a record the turn did not reach; `{record}` is its label and
    /// `{question}` the question.
    pub elsewhere: LocalizedText,
    /// The question a reply ends on when it asks nothing else.
    pub go_on: LocalizedText,
    /// A question the last reply asked too; `{question}` is the question.
    pub again: LocalizedText,
    /// The offer to open a record none of exists; `{noun}` is what one is called.
    pub open_new: LocalizedText,
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
            elsewhere: LocalizedText::new("{record}: {question}"),
            go_on: LocalizedText::new("What would you like to do next?"),
            again: LocalizedText::new(
                "I still need this to go on. {question} Or tell me what else you would like to do.",
            ),
            open_new: LocalizedText::new("Open a new {noun}."),
        }
    }
}

impl Default for AskCopy {
    fn default() -> Self {
        Self::standard()
    }
}

crate::copy::server_copy!(
    AskCopy,
    [
        value,
        refused_value,
        obligation,
        contested,
        elsewhere,
        go_on,
        again,
        open_new
    ]
);

/// The built-in Italian of [`AskCopy`], by field.
const ITALIAN: &[(&str, &str)] = &[
    ("value", "Che cosa metto come {what}?"),
    (
        "refused_value",
        "{because} Che cosa metto come {what}, invece?",
    ),
    ("obligation", "Manca ancora: {what}."),
    ("contested", "{what} Come dovrebbe essere, invece?"),
    ("elsewhere", "{record}: {question}"),
    ("go_on", "Cosa vuoi fare adesso?"),
    (
        "again",
        "Mi serve ancora per andare avanti. {question} Oppure dimmi cos'altro vuoi fare.",
    ),
    ("open_new", "Aprire un nuovo {noun}."),
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
    /// Cards earlier turns left open.
    pub open_cards: &'a [Interaction],
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
    pub next_steps: &'a [(CaseRef, Vec<turnframe_core::flow::NextStep>)],
    /// What the last reply asked.
    pub asked_before: &'a [Expectation],
    /// Records to offer to open, none existing: the record still to create, the operation
    /// opening one, and what one is called.
    pub openings: &'a [(CaseRef, turnframe_core::ids::OperationKey, String)],
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
                again: false,
                elsewhere: false,
                expectation: None,
                about: None,
            });
        }
        let waiting = self.facts.iter().find_map(|fact| match fact {
            NarratableFact::ValueNeeded {
                case_ref,
                operation,
                arguments,
                reason,
            } => Some((
                case_ref.clone(),
                operation.clone(),
                arguments.join(", "),
                reason.clone(),
            )),
            _ => None,
        });
        if let Some((case_ref, operation, what, because)) = waiting {
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
                about: case_ref.map(|case_ref| (case_ref.key(), operation)),
                what,
                because,
                question,
                again: false,
                elsewhere: false,
                // The waiting act is recorded as its own expectation already.
                expectation: None,
            });
        }
        let reached = |case_ref: &CaseRef| self.touched.contains(&case_ref.key());
        let card_next = self
            .interactions
            .iter()
            .chain(self.open_cards)
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
        let record = self.label(&view.case_ref);
        // A record the turn did not reach is not the one the reply talks about: name it.
        let elsewhere = !reached(&view.case_ref) && record.is_some();
        let question = match record.as_deref().filter(|_| elsewhere) {
            Some(label) => fill(
                &self.copy.elsewhere,
                self.locale,
                &[("record", label), ("question", &question)],
            ),
            None => question,
        };
        Some(Ask {
            record,
            question,
            elsewhere,
            about: Some((view.case_ref.key(), what.clone())),
            expectation: Some(expectation),
            what,
            because: None,
            again: false,
        })
    }

    /// Where each of `keys` stands, for the ones in view with anything to say.
    pub fn standing(&self, keys: &[CaseKey]) -> Vec<Standing> {
        keys.iter()
            .filter_map(|key| {
                let view = self.views.iter().find(|view| view.case_ref.key() == *key)?;
                let holds: Vec<String> = view
                    .state
                    .iter()
                    .map(|field| {
                        format!(
                            "{}: {}",
                            field.field.replace('_', " "),
                            spoken(&field.value)
                        )
                    })
                    .collect();
                let needs: Vec<String> = view
                    .obligations
                    .iter()
                    .map(|obligation| match &obligation.sentence {
                        Some(sentence) => sentence.resolve(self.locale).to_owned(),
                        None => obligation_words(&obligation.value),
                    })
                    .collect();
                (!holds.is_empty() || !needs.is_empty()).then(|| Standing {
                    record: self
                        .label(&view.case_ref)
                        .unwrap_or_else(|| view.case_ref.case_id.to_string()),
                    holds,
                    needs,
                })
            })
            .collect()
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
        // A card this turn raised, else one an earlier turn left open on a record it reached.
        let shown = self.interactions.first().or_else(|| {
            self.open_cards
                .iter()
                .find(|card| card.blocking && self.touched.contains(&card.case_ref.key()))
        });
        let card = shown.map(|card| {
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
        let mut ask = self.ask();
        // Asked again with no refusal to explain it: the reply says so and offers the rest.
        if let Some(ask) = ask.as_mut().filter(|ask| ask.because.is_none()) {
            ask.again = ask.about.as_ref().is_some_and(|about| {
                self.asked_before
                    .iter()
                    .any(|before| asked(before).as_ref() == Some(about))
            });
            if ask.again {
                ask.question = fill(&self.copy.again, locale, &[("question", &ask.question)]);
            }
        }
        let open = ask.as_ref().is_none_or(|ask| ask.again);
        let offers: Vec<turnframe_core::response::Offer> = if open && card.is_none() {
            self.touched
                .iter()
                .find_map(|key| {
                    self.next_steps
                        .iter()
                        .find(|(case, steps)| case.key() == *key && !steps.is_empty())
                })
                .map(|(case, steps)| {
                    steps
                        .iter()
                        .map(|step| turnframe_core::response::Offer {
                            case_ref: case.clone(),
                            operation: step.operation.clone(),
                            words: step.words.resolve(locale).to_owned(),
                            arguments: step.arguments.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut offers = offers;
        if open && card.is_none() {
            offers.extend(self.openings.iter().map(|(case_ref, operation, noun)| {
                turnframe_core::response::Offer {
                    case_ref: case_ref.clone(),
                    operation: operation.clone(),
                    words: fill(&self.copy.open_new, locale, &[("noun", noun)]),
                    arguments: serde_json::Map::new(),
                }
            }));
        }
        let next: Vec<String> = offers.iter().map(|offer| offer.words.clone()).collect();
        let closing = (ask.is_none() && card.is_none() && next.is_empty())
            .then(|| self.copy.go_on.resolve(locale).to_owned());
        TurnOutcome {
            done,
            not_done,
            disputes: self.disputes.to_vec(),
            starting: self.started.iter().map(ToString::to_string).collect(),
            ask,
            card,
            next,
            closing,
            offers,
            standing: Vec::new(),
        }
    }
}

/// A value as words: a string as it is, a list as its items.
fn spoken(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items.iter().map(spoken).collect::<Vec<_>>().join("; "),
        other => other.to_string(),
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
            open_cards: &[],
            views: &[],
            touched: &[],
            beside: &[],
            labels: &[],
            disputes: &[],
            started: &[],
            contested: &[],
            next_steps: &[],
            asked_before: &[],
            openings: &[],
            locale,
            copy,
        }
    }

    #[test]
    fn an_ask_the_last_reply_asked_says_so_and_offers_the_next_steps_beside_it() {
        let case_ref = CaseRef::new("sample", "s-1", CaseRevision(2));
        let view = ErasedWorkflowView {
            case_ref: case_ref.clone(),
            workflow_version: turnframe_core::ids::WorkflowVersion::from("1"),
            phase: serde_json::json!("collecting"),
            phase_ownership: turnframe_core::flow::PhaseOwnership::User,
            obligations: vec![turnframe_core::flow::ErasedObligation {
                id: turnframe_core::flow::ObligationId("\"a\"".to_owned()),
                value: serde_json::json!("a"),
                sentence: Some(LocalizedText::new("What is A?")),
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
        let next_steps = [(
            case_ref.clone(),
            vec![turnframe_core::flow::NextStep::new(
                "sample.add",
                LocalizedText::new("Add another item."),
            )],
        )];
        let before = [Expectation::AwaitingObligation {
            case_ref: CaseRef::new("sample", "s-1", CaseRevision(1)),
            obligation: "What is A?".to_owned(),
        }];
        let mut material = material(&[], &copy, &locale);
        material.views = &views;
        material.touched = &touched;
        material.next_steps = &next_steps;

        let first = material.outcome();
        assert!(!first.ask.as_ref().expect("an ask").again);
        assert!(first.next.is_empty(), "what is owed comes first");

        material.asked_before = &before;
        let again = material.outcome();
        let ask = again.ask.expect("the same ask");
        assert!(ask.again);
        assert_eq!(
            ask.question,
            "I still need this to go on. What is A? Or tell me what else you would like to do."
        );
        assert_eq!(again.next, vec!["Add another item."]);
    }

    #[test]
    fn a_record_stands_as_what_it_holds_and_what_it_still_needs() {
        let case_ref = CaseRef::new("sample", "s-1", CaseRevision(1));
        let view = ErasedWorkflowView {
            case_ref: case_ref.clone(),
            workflow_version: turnframe_core::ids::WorkflowVersion::from("1"),
            phase: serde_json::json!("collecting"),
            phase_ownership: turnframe_core::flow::PhaseOwnership::User,
            obligations: vec![turnframe_core::flow::ErasedObligation {
                id: turnframe_core::flow::ObligationId("\"b\"".to_owned()),
                value: serde_json::json!("set_b"),
                sentence: None,
                act: None,
            }],
            blocking_interaction: None,
            notices: Vec::new(),
            outcome: None,
            state: vec![
                turnframe_core::flow::StateField::new("the_a", serde_json::json!("X")),
                turnframe_core::flow::StateField::new("items", serde_json::json!(["one", "two"])),
            ],
        };
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        let views = [view];
        let labels = [CaseLabel {
            case_ref: case_ref.clone(),
            label: "S 1".to_owned(),
        }];
        let mut material = material(&[], &copy, &locale);
        material.views = &views;
        material.labels = &labels;
        let standing = material.standing(&[case_ref.key()]);
        assert_eq!(
            standing,
            vec![Standing {
                record: "S 1".to_owned(),
                holds: vec!["the a: X".to_owned(), "items: one; two".to_owned()],
                needs: vec!["set b".to_owned()],
            }]
        );
        assert_eq!(
            standing[0].line().as_deref(),
            Some("S 1: the a: X, items: one; two.")
        );
    }

    #[test]
    fn a_value_asked_again_after_a_refusal_gives_the_refusal_as_its_reason() {
        let case_ref = CaseRef::new("trip", "trip-1", CaseRevision(1));
        let facts = [NarratableFact::ValueNeeded {
            case_ref: Some(case_ref.clone()),
            operation: "trip.set_name".to_owned(),
            arguments: vec!["subject".to_owned()],
            reason: Some("Too long.".to_owned()),
        }];
        let act = crate::resume::card_act(
            turnframe_core::understanding::ActAction::Apply {
                operation: "trip.set_name".into(),
            },
            turnframe_core::understanding::ActTarget::Card,
        );
        let before = [Expectation::AwaitingValue {
            act: Box::new(act),
            case_ref: Some(case_ref),
            missing: vec!["subject".to_owned()],
        }];
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        let mut material = material(&facts, &copy, &locale);
        material.asked_before = &before;
        let ask = material.outcome().ask.expect("an ask");
        assert!(!ask.again, "the refusal is the reason already given");
        assert_eq!(
            ask.question,
            "Too long. What should the subject be instead?"
        );
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
            CaseRef::new("sample", "s-1", CaseRevision(1)),
            vec![
                turnframe_core::flow::NextStep::new(
                    "sample.add",
                    LocalizedText::new("Add another item."),
                ),
                turnframe_core::flow::NextStep::new("sample.send", LocalizedText::new("Send it.")),
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

    fn owing(case_id: &str, owed: bool) -> ErasedWorkflowView {
        ErasedWorkflowView {
            case_ref: CaseRef::new("sample", case_id, CaseRevision(1)),
            workflow_version: turnframe_core::ids::WorkflowVersion::from("1"),
            phase: serde_json::json!("collecting"),
            phase_ownership: turnframe_core::flow::PhaseOwnership::User,
            obligations: owed
                .then(|| turnframe_core::flow::ErasedObligation {
                    id: turnframe_core::flow::ObligationId("\"a\"".to_owned()),
                    value: serde_json::json!("a"),
                    sentence: Some(LocalizedText::new("What is A?")),
                    act: None,
                })
                .into_iter()
                .collect(),
            blocking_interaction: None,
            notices: Vec::new(),
            outcome: None,
            state: Vec::new(),
        }
    }

    #[test]
    fn an_obligation_of_a_record_the_turn_did_not_reach_names_that_record() {
        let views = [owing("s-1", false), owing("s-2", true)];
        let touched = [views[0].case_ref.key()];
        let beside = [views[1].case_ref.key()];
        let labels = [CaseLabel {
            case_ref: views[1].case_ref.clone(),
            label: "Sample 2".to_owned(),
        }];
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        let mut material = material(&[], &copy, &locale);
        material.views = &views;
        material.touched = &touched;
        material.beside = &beside;
        material.labels = &labels;
        let ask = material
            .outcome()
            .ask
            .expect("the other record's obligation");
        assert!(ask.elsewhere);
        assert_eq!(ask.question, "Sample 2: What is A?");
    }

    #[test]
    fn an_obligation_of_a_record_the_turn_reached_is_asked_as_written() {
        let views = [owing("s-1", true)];
        let touched = [views[0].case_ref.key()];
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        let mut material = material(&[], &copy, &locale);
        material.views = &views;
        material.touched = &touched;
        let ask = material.outcome().ask.expect("its obligation");
        assert!(!ask.elsewhere);
        assert_eq!(ask.question, "What is A?");
    }

    #[test]
    fn nothing_to_say_is_silence() {
        let copy = AskCopy::english();
        let locale = Locale::from("en-GB");
        assert!(material(&[], &copy, &locale).outcome().is_silent());
    }
}
