//! What the console prints: what was understood, what the runtime did, and the case.

use turnframe::flow::{CaseRef, WorkflowDefinition, WorkflowExecutor};
use turnframe::ids::AccountId;
use turnframe::interaction::Interaction;
use turnframe::locale::Locale;
use turnframe::replay::{ReplayRecord, TaskVerdict};
use turnframe::response::{AssistantTurn, ResponseBlock};
use turnframe::testing::workflows::traveler::TravelerWorkflow;
use turnframe::testing::workflows::trip::TripWorkflow;

use crate::directory::Records;
use crate::style;
use turnframe::understanding::{
    ActAction, ActStatus, ActTarget, ArgumentValue, RecordValue, WordRange,
};

fn words(text: &str, range: WordRange) -> &str {
    text.get(range.start..range.end).unwrap_or_default()
}

/// What the message was understood to say, and every model call that failed.
pub fn understood(record: &ReplayRecord, text: &str) {
    for task in &record.tasks {
        match &task.verdict {
            TaskVerdict::Rejected { code, reason } => println!(
                "  {}",
                style::warning(&format!("! {} rejected ({code}): {reason}", task.task_id))
            ),
            TaskVerdict::Failed { code } => println!(
                "  {}",
                style::warning(&format!("! {} failed ({code})", task.task_id))
            ),
            _ => {}
        }
    }
    let Some(understanding) = record.understanding.as_ref() else {
        return;
    };
    if let Some(unreadable) = understanding.unreadable.as_ref() {
        println!(
            "  {}",
            style::warning(&format!("understood nothing: {unreadable:?}"))
        );
    }
    for act in &understanding.acts {
        let action = match &act.action {
            ActAction::Apply { operation } => operation.to_string(),
            ActAction::Start { workflow } => format!("start {workflow}"),
        };
        let target = match &act.target {
            ActTarget::Record { .. } => "a record in view".to_owned(),
            ActTarget::New { workflow } => format!("a new {workflow}"),
            ActTarget::SameTurn { act } => format!("what {act} creates"),
            ActTarget::Card => "the card on screen".to_owned(),
            ActTarget::NotListed { workflow, .. } => format!("a {workflow} not in view"),
            ActTarget::Ambiguous { candidates } => format!("one of {} records", candidates.len()),
            _ => "no record".to_owned(),
        };
        let arguments: Vec<String> = act
            .arguments
            .iter()
            .map(|(name, argument)| match &argument.value {
                ArgumentValue::Json(value) => format!("{name} = {value}"),
                ArgumentValue::Record(RecordValue::Record { .. }) => {
                    format!("{name} = a record in view")
                }
                ArgumentValue::Record(RecordValue::SameTurn { act }) => {
                    format!("{name} = the record {act} opens")
                }
                ArgumentValue::Record(RecordValue::Named { named, .. }) => {
                    format!("{name} = «{named}», not in view")
                }
            })
            .collect();
        let status = match &act.status {
            ActStatus::Ready => String::new(),
            ActStatus::NeedsValue { arguments, .. } => {
                format!(", asks for {}", arguments.join(", "))
            }
            ActStatus::Held { because } => format!(", held by {because}"),
            _ => String::new(),
        };
        let given = if arguments.is_empty() {
            String::new()
        } else {
            format!(" ({})", arguments.join(", "))
        };
        println!(
            "  {}{}",
            style::tag("understood"),
            style::muted(&format!("{} {action} on {target}{given}{status}", act.id))
        );
    }
    for question in &understanding.questions {
        println!(
            "  {}{}",
            style::tag("question"),
            style::muted(&format!(
                "«{}» {:?}{} ({:?})",
                words(text, question.words),
                question.topic,
                if question.subjects.is_empty() {
                    String::new()
                } else {
                    format!(" about {:?}", question.subjects)
                },
                question.basis
            ))
        );
    }
    for constraint in &understanding.constraints {
        println!(
            "  {}{}",
            style::tag("constraint"),
            style::muted(&format!(
                "{:?} «{}»",
                constraint.kind,
                words(text, constraint.words)
            ))
        );
    }
    if let Some(answer) = understanding.card_answer.as_ref() {
        println!(
            "  {}{}",
            style::tag("card"),
            style::muted(&format!("answered {}", answer.option))
        );
    }
    for missed in &understanding.not_understood {
        println!(
            "  {}",
            style::warning(&format!(
                "not understood «{}» ({:?})",
                words(text, missed.words),
                missed.reason
            ))
        );
    }
    if !record.act_outcomes.is_empty() {
        println!(
            "  {}{}",
            style::tag("runtime"),
            style::muted(&record.act_outcomes.join(", "))
        );
    }
    if let Some(budget) = record.budget.as_ref() {
        println!(
            "  {}{}",
            style::tag("spent"),
            style::muted(&format!(
                "{} call(s), {} prompt tokens, effort {}",
                budget.model_calls, budget.prompt_tokens, record.effort
            ))
        );
    }
}

/// The blocks of the turn, as the user reads them: the only part a real surface shows.
pub fn turn(turn: &AssistantTurn, locale: &Locale) {
    println!();
    // What the turn recorded, in grey: the reply below is what the user reads of it.
    for block in &turn.blocks {
        let (kind, text) = match block {
            ResponseBlock::Answer(block) => ("answer", block.text.clone()),
            // Rendered from committed events: the only part that may claim anything.
            ResponseBlock::Receipt(block) => (
                "receipt",
                format!(
                    "{}: {}",
                    block.receipt.title.resolve(locale),
                    block.receipt.body.resolve(locale)
                ),
            ),
            ResponseBlock::Notice(block) => ("notice", block.text.resolve(locale).to_owned()),
            // A card is printed whole after the turn, options and all, blocking or not.
            _ => continue,
        };
        println!("  {}{}", style::tag(kind), style::muted(&text));
    }
    let reply = turn.blocks.iter().find_map(|block| match block {
        ResponseBlock::Transition(reply) => Some(reply.text.as_str()),
        _ => None,
    });
    if let Some(reply) = reply {
        let mut lines = reply.lines();
        let first = lines.next().unwrap_or_default();
        println!("  {}{}", style::block("assistant"), style::shown(first));
        for line in lines {
            println!("            {}", style::shown(line));
        }
    }
    // The next steps travel as data beside the reply, as a surface would show them.
    for offer in &turn.offers {
        println!("  {}{}", style::tag("next"), style::muted(&offer.words));
    }
    println!();
}

/// The card's options, numbered, so a bare number presses one.
pub fn card(card: &Interaction, locale: &Locale) {
    println!(
        "  {}{}",
        style::block("card"),
        style::shown(card.payload.title.resolve(locale))
    );
    if let Some(body) = &card.payload.body {
        for line in body.resolve(locale).lines() {
            println!("            {}", style::shown(line));
        }
    }
    for entry in &card.payload.review_entries {
        let side = |value: &turnframe::interaction::FieldValue| match value {
            turnframe::interaction::FieldValue::Present(value) => value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned),
            _ => "nothing".to_owned(),
        };
        println!(
            "            {}",
            style::shown(&format!(
                "{}: {} → {}",
                entry.label.resolve(locale),
                side(&entry.before),
                side(&entry.after)
            ))
        );
    }
    for (index, option) in card.payload.options.iter().enumerate() {
        println!(
            "            {}",
            style::shown(&format!("{}. {}", index + 1, option.label.resolve(locale)))
        );
    }
    println!("  {}\n", style::muted("(type the number to press it)"));
}

/// Each record as its projector sees it: the phase, and what is still open.
pub async fn state(records: &Records, account: &AccountId) {
    let labelled = records.labelled(account);
    if labelled.is_empty() {
        println!(
            "  {}\n",
            style::muted("No records yet: ask to register a traveler or to open a trip.")
        );
    }
    for (key, label) in labelled {
        let (phase, open) = if key.workflow.as_str() == crate::TRAVELER {
            let Ok(loaded) = records.travelers.load(account, &key.case_id).await else {
                continue;
            };
            let case_ref = CaseRef::new(key.workflow.clone(), key.case_id.clone(), loaded.revision);
            let view = TravelerWorkflow::default().project(case_ref, loaded.value.as_ref());
            (
                format!("{:?}", view.phase),
                format!("{:?}", view.obligations),
            )
        } else {
            let Ok(loaded) = records.trips.load(account, &key.case_id).await else {
                continue;
            };
            let case_ref = CaseRef::new(key.workflow.clone(), key.case_id.clone(), loaded.revision);
            let view = TripWorkflow::default().project(case_ref, loaded.value.as_ref());
            let traveler = loaded
                .value
                .as_ref()
                .and_then(|state| state.traveler.as_ref())
                .map_or("-", |traveler| traveler.display_name.as_str())
                .to_owned();
            println!("  {}", style::heading(&format!("{label} (for {traveler})")));
            println!(
                "  {}{}",
                style::tag("phase"),
                style::muted(&format!("{:?}", view.phase))
            );
            println!(
                "  {}{}\n",
                style::tag("open"),
                style::muted(&format!("{:?}", view.obligations))
            );
            continue;
        };
        println!("  {}", style::heading(&label));
        println!("  {}{}", style::tag("phase"), style::muted(&phase));
        println!("  {}{}\n", style::tag("open"), style::muted(&open));
    }
}

pub fn help() {
    println!(
        "\n  /state   each record as its projector sees it\n  \
         /effort  low, medium or high: how hard each turn works to read you (medium)\n  \
         /help    this\n  \
         /quit    leave\n  \
         <number> press that button, when a card is on screen\n  \
         anything else is a message to the assistant\n"
    );
}
