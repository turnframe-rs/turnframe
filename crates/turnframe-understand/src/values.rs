//! From an extraction to the operation's values: slicing words, evaluating dates and
//! amounts, resolving handles. Every failure is structural and worded for a repair.

use std::collections::BTreeMap;

use turnframe_core::operation::{Money, ValueShape};
use turnframe_core::understanding::{
    ArgumentValue, Excerpt, MessageRef, RecordValue, UnderstoodArgument,
};
use turnframe_tasks::StructuralError;

use crate::input::UnderstandingInput;
use crate::tasks::extract::{BY_NAME, CURRENT, ExtractInput, Extraction, Given};
use crate::tasks::{check_one_of, not_one_of, out_of_range};
use crate::words::{Span, Words};

/// An extraction turned into values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    /// The arguments given, by name.
    pub arguments: BTreeMap<String, UnderstoodArgument>,
    /// The arguments not given, in declaration order.
    pub not_given: Vec<String>,
    /// Of those, the ones pointed at in words another part of the message holds, with
    /// those words: that part's value, which this act may still find elsewhere.
    pub elsewhere: Vec<(String, Span)>,
}

/// Turns `output` into values, or says what is structurally wrong with it.
///
/// # Errors
///
/// A [`StructuralError`] for a missing or unknown argument, a value of the wrong kind,
/// a pointer outside its message, a date that does not exist or an amount that is not
/// one.
pub fn convert(
    turn: &UnderstandingInput,
    input: &ExtractInput<'_>,
    output: &Extraction,
) -> Result<Extracted, StructuralError> {
    let extracted = converted(turn, input, output)?;
    shares_no_words(input, &extracted)?;
    Ok(extracted)
}

/// Whether `given` chooses one of the records offered: a listed one, or one another act of
/// this message creates.
fn chosen(input: &ExtractInput<'_>, given: &Given) -> bool {
    let Given::Record { record, .. } = given else {
        return false;
    };
    input
        .record_choices
        .values()
        .flat_map(|choices| choices.iter())
        .any(|choice| &choice.handle == record)
}

/// Whether `span` belongs to another part of the message: outside the request's own words
/// and the words it continues, and not reached from them through words no part holds.
fn elsewhere(input: &ExtractInput<'_>, span: Span) -> bool {
    let within = |outer: Span| outer.from <= span.from && span.to <= outer.to;
    if within(input.words) || input.continues.is_some_and(within) {
        return false;
    }
    // A value of this part running on into a neighbour asking for the same operation is one
    // value the segmentation cut in two.
    let mut whole = input.words;
    for kin in &input.kin {
        if kin.to.saturating_add(1) == whole.from {
            whole = Span::new(kin.from, whole.to);
        } else if whole.to.saturating_add(1) == kin.from {
            whole = Span::new(whole.from, kin.to);
        }
    }
    if within(whole) && span.from <= input.words.to && input.words.from <= span.to {
        return false;
    }
    let reach = Span::new(span.from.min(input.words.from), span.to.max(input.words.to));
    input
        .others
        .iter()
        .any(|other| other.from <= reach.to && reach.from <= other.to)
}

/// One word is one value: two arguments pointing at the same words of a message are two
/// readings of one value, and one of them is wrong. A value that may be deduced points at
/// the words that imply it, which may be those of the value they state.
fn shares_no_words(input: &ExtractInput<'_>, extracted: &Extracted) -> Result<(), StructuralError> {
    let deduced = |name: &str| {
        input.arguments.iter().any(|argument| {
            argument.name == name
                && matches!(
                    argument.source,
                    turnframe_core::operation::ArgumentSource::Inferred
                )
        })
    };
    let pointed: Vec<(&String, &Excerpt)> = extracted
        .arguments
        .iter()
        .filter_map(|(name, argument)| argument.excerpt.as_ref().map(|excerpt| (name, excerpt)))
        .collect();
    for (at, (name, excerpt)) in pointed.iter().enumerate() {
        for (other, theirs) in &pointed[at + 1..] {
            if deduced(name) != deduced(other) {
                continue;
            }
            let shared = excerpt.message == theirs.message
                && excerpt.words.first <= theirs.words.last
                && theirs.words.first <= excerpt.words.last;
            if shared {
                return Err(StructuralError::new(
                    "shared_words",
                    format!(
                        "`{name}` and `{other}` point at the same words: each value takes words \
                         of its own, and a value the message only implies points at the words \
                         that imply it"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn converted(
    turn: &UnderstandingInput,
    input: &ExtractInput<'_>,
    output: &Extraction,
) -> Result<Extracted, StructuralError> {
    let asked: Vec<String> = input.arguments.iter().map(|a| a.name.clone()).collect();
    if let Some(unknown) = output.arguments.keys().find(|name| !asked.contains(name)) {
        return Err(not_one_of("argument", unknown, &asked));
    }
    let messages = input.messages(turn);
    let mut extracted = Extracted::default();
    for argument in &input.arguments {
        let name = &argument.name;
        let given = output.arguments.get(name).ok_or_else(|| {
            StructuralError::new("missing_argument", format!("`arguments.{name}` is missing"))
        })?;
        let Some((message, span)) = given.pointer() else {
            extracted.not_given.push(name.clone());
            continue;
        };
        check_one_of(&format!("{name}.message"), message, &messages)?;
        let (reference, words) = message_words(turn, message)
            .ok_or_else(|| not_one_of(&format!("{name}.message"), message, &messages))?;
        let span = match given {
            Given::Words { text, .. } if !text.trim().is_empty() => {
                narrow(name, words, span, text)?
            }
            _ => span,
        };
        let range = words
            .range(span)
            .map_err(|_| out_of_range(name, span, words))?;
        // A value in another part of this message is that part's act's, not this one's; a
        // record offered is chosen by its handle, and one named by its name, wherever its
        // words are: a record is looked up, never copied.
        let by_name = matches!(given, Given::Record { record, .. } if record == BY_NAME);
        if message == CURRENT && elsewhere(input, span) && !chosen(input, given) && !by_name {
            extracted.not_given.push(name.clone());
            extracted.elsewhere.push((name.clone(), span));
            continue;
        }
        let value = value_of(turn, input, name, &argument.shape, given, words, span)?;
        if !matches!(
            argument.source,
            turnframe_core::operation::ArgumentSource::Inferred
        ) {
            says_its_number(name, given, words, span)?;
        }
        extracted.arguments.insert(
            name.clone(),
            UnderstoodArgument {
                value,
                excerpt: Some(Excerpt {
                    message: reference,
                    words: range,
                }),
            },
        );
    }
    Ok(extracted)
}

/// A stated number is one its words say, when they hold any: «450» pointed at «500 euros» is
/// a pointer at another number. Words that say it in letters are not read here.
fn says_its_number(
    name: &str,
    given: &Given,
    words: &Words,
    span: Span,
) -> Result<(), StructuralError> {
    let stated = match given {
        Given::Money { amount, .. } => amount.replace(',', ".").trim().parse::<f64>().ok(),
        Given::Value { value, .. } => value.as_f64(),
        _ => None,
    };
    let (Some(stated), Ok(said)) = (stated, words.slice(span)) else {
        return Ok(());
    };
    let numbers = numbers_in(said);
    if numbers.is_empty() || numbers.iter().any(|n| (n - stated).abs() < 1e-9) {
        return Ok(());
    }
    let (from, to) = span.shown();
    Err(StructuralError::new(
        "number_not_in_words",
        format!(
            "`arguments.{name}` is {stated}, which words {from} to {to} («{said}») do not say; \
             point at the words that say it"
        ),
    ))
}

/// Every value the numbers written in digits in `text` may mean, reading a lone comma or
/// full stop as decimal or thousands mark, and the last of two kinds as the decimal one.
fn numbers_in(text: &str) -> Vec<f64> {
    let mut values = Vec::new();
    let tokens = text
        .split(|c: char| !(c.is_ascii_digit() || c == ',' || c == '.'))
        .map(|token| token.trim_matches(|c: char| c == ',' || c == '.'))
        .filter(|token| token.starts_with(|c: char| c.is_ascii_digit()));
    for token in tokens {
        let commas = token.contains(',');
        let stops = token.contains('.');
        let readings: Vec<String> = match (commas, stops) {
            (false, false) => vec![token.to_owned()],
            (true, false) => vec![token.replace(',', "."), token.replace(',', "")],
            (false, true) => vec![token.to_owned(), token.replace('.', "")],
            (true, true) => {
                let last = token.rfind([',', '.']).unwrap_or(0);
                let (whole, part) = token.split_at(last);
                vec![format!("{}.{}", whole.replace([',', '.'], ""), &part[1..])]
            }
        };
        values.extend(
            readings
                .iter()
                .filter_map(|reading| reading.parse::<f64>().ok()),
        );
    }
    values
}

/// The words of `span` that `copied` repeats: the value alone, without the words that
/// name its field or join it to the rest. A copy the words pointed at do not hold is where
/// the message holds it, when it holds it once.
fn narrow(name: &str, words: &Words, span: Span, copied: &str) -> Result<Span, StructuralError> {
    let pointed = words
        .slice(span)
        .map_err(|_| out_of_range(name, span, words))?;
    words
        .narrow(span, copied)
        .or_else(|| words.only_place(copied))
        .ok_or_else(|| {
        let (from, to) = span.shown();
        StructuralError::new(
            "text_not_pointed_at",
            format!(
                "`arguments.{name}.text` is «{copied}», which is not among words {from} to {to} \
                 («{pointed}»); copy the value's words exactly, and point at the words that hold them"
            ),
        )
    })
}

fn message_words<'a>(turn: &'a UnderstandingInput, name: &str) -> Option<(MessageRef, &'a Words)> {
    if name == CURRENT {
        return Some((MessageRef::Current, &turn.message));
    }
    let index = name
        .strip_prefix('m')?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)?;
    let message = turn.transcript.get(index)?;
    Some((MessageRef::Earlier { index }, &message.words))
}

fn wrong_kind(name: &str, shape: &ValueShape) -> StructuralError {
    let expected = match shape {
        ValueShape::Text { written: false } => "words",
        ValueShape::Text { written: true } => "written",
        ValueShape::Date { .. } => "date",
        ValueShape::Money => "money",
        ValueShape::Record { .. } => "record",
        _ => "value",
    };
    StructuralError::new(
        "wrong_kind",
        format!("`arguments.{name}` takes kind {expected} or not_given"),
    )
}

/// `text` without one pair of quotes around the whole of it.
fn unquoted(text: &str) -> &str {
    const PAIRS: [(char, char); 5] = [('"', '"'), ('\'', '\''), ('«', '»'), ('“', '”'), ('‘', '’')];
    PAIRS
        .iter()
        .find_map(|(open, close)| {
            let inner = text.strip_prefix(*open)?.strip_suffix(*close)?;
            (!inner.trim().is_empty()).then(|| inner.trim())
        })
        .unwrap_or(text)
}

fn value_of(
    turn: &UnderstandingInput,
    input: &ExtractInput<'_>,
    name: &str,
    shape: &ValueShape,
    given: &Given,
    words: &Words,
    span: Span,
) -> Result<ArgumentValue, StructuralError> {
    let json = ArgumentValue::Json;
    match (shape, given) {
        (ValueShape::Text { written: false }, Given::Words { text: copied, .. }) => {
            let text = words
                .slice(span)
                .map_err(|_| out_of_range(name, span, words))?;
            // The punctuation that joins a value to the next words is not the value, nor
            // are the quotes that mark it off, nor the mark ending a sentence the copy drops;
            // a mark the joining punctuation follows ends no sentence.
            let joined = text.trim_end_matches([',', ';', ':']);
            let ends_sentence = joined.len() == text.len();
            let mut text = joined;
            let ends = ['.', '!', '?'];
            if ends_sentence && !copied.trim().is_empty() && !copied.trim_end().ends_with(ends) {
                text = text.trim_end_matches(ends);
            }
            let text = unquoted(text);
            Ok(json(serde_json::Value::from(text)))
        }
        (ValueShape::Text { written: true }, Given::Written { text, .. }) => {
            if text.trim().is_empty() {
                return Err(StructuralError::new(
                    "empty_text",
                    format!("`arguments.{name}.text` is empty; give not_given instead"),
                ));
            }
            Ok(json(serde_json::Value::from(text.trim())))
        }
        (ValueShape::Enum { values }, Given::Value { value, .. }) => {
            let text = value.as_str().unwrap_or_default();
            check_one_of(&format!("{name}.value"), text, values)?;
            Ok(json(value.clone()))
        }
        (ValueShape::Integer, Given::Value { value, .. }) if value.is_i64() || value.is_u64() => {
            Ok(json(value.clone()))
        }
        (ValueShape::Number, Given::Value { value, .. }) if value.is_number() => {
            Ok(json(value.clone()))
        }
        (ValueShape::Bool, Given::Value { value, .. }) if value.is_boolean() => {
            Ok(json(value.clone()))
        }
        (ValueShape::Structured, Given::Value { value, .. }) => Ok(json(value.clone())),
        (ValueShape::Date { direction }, Given::Date { date, .. }) => {
            let day = date.evaluate(turn.today, *direction).map_err(|error| {
                StructuralError::new(
                    "no_such_date",
                    format!("`arguments.{name}.date`: {error}; give the date the user said"),
                )
            })?;
            Ok(json(serde_json::Value::from(day.to_string())))
        }
        (
            ValueShape::Money,
            Given::Money {
                amount, currency, ..
            },
        ) => {
            let money = Money::parse(amount, currency).map_err(|error| {
                StructuralError::new("not_money", format!("`arguments.{name}`: {error}"))
            })?;
            Ok(json(serde_json::to_value(money).unwrap_or_default()))
        }
        (
            ValueShape::Record { workflow },
            Given::Record {
                record,
                name: named,
                ..
            },
        ) if record == BY_NAME => {
            if named.trim().is_empty() {
                return Err(StructuralError::new(
                    "missing_name",
                    format!("`arguments.{name}.name` must be the record's name when it is by_name"),
                ));
            }
            // A name that is the whole name of one record in view is that record.
            let words = |text: &str| {
                text.split_whitespace()
                    .map(str::to_lowercase)
                    .collect::<Vec<_>>()
            };
            let listed: Vec<&RecordValue> = input
                .record_choices
                .get(name)
                .into_iter()
                .flatten()
                .map(|choice| &choice.value)
                .filter(|value| match value {
                    RecordValue::Record { token } => turn
                        .record(token)
                        .is_some_and(|(_, record)| words(&record.label) == words(named)),
                    _ => false,
                })
                .collect();
            if let [record] = listed.as_slice() {
                return Ok(ArgumentValue::Record((*record).clone()));
            }
            Ok(ArgumentValue::Record(RecordValue::Named {
                workflow: workflow.clone(),
                named: named.trim().to_owned(),
            }))
        }
        (ValueShape::Record { .. }, Given::Record { record, .. }) => {
            let choices = input
                .record_choices
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let handles: Vec<String> = choices.iter().map(|c| c.handle.clone()).collect();
            let choice = choices
                .iter()
                .find(|choice| &choice.handle == record)
                .ok_or_else(|| not_one_of(&format!("{name}.record"), record, &handles))?;
            Ok(ArgumentValue::Record(choice.value.clone()))
        }
        _ => Err(wrong_kind(name, shape)),
    }
}
