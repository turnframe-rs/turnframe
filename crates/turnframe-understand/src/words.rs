//! The user's words, numbered, so a task can point at them.
//!
//! A message is split on whitespace, punctuation staying attached to its word, and
//! grouped into sentences after terminal punctuation. A pointer is a closed range of
//! word indices; code slices the exact words and their byte offsets. A value's copied
//! words may narrow a pointer to the words inside it, never move it: nothing a model
//! writes is looked for outside the words it pointed at.

use serde::{Deserialize, Serialize};
use turnframe_core::understanding::WordRange;

/// One word of a message and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The word as written, punctuation included.
    pub text: String,
    /// Byte offset of its first character.
    pub start: usize,
    /// Byte offset just past its last character.
    pub end: usize,
}

/// A message split into numbered words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    text: String,
    words: Vec<Word>,
}

/// A closed range of word indices, `from` to `to` inclusive, counted from 0.
///
/// A model is shown and answers word numbers counted from 1, the way people count;
/// [`Span::shown`] and the serde form are that numbering, and nothing else is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    /// First word.
    pub from: usize,
    /// Last word.
    pub to: usize,
}

impl Span {
    /// A span from `from` to `to` inclusive.
    #[must_use]
    pub const fn new(from: usize, to: usize) -> Self {
        Self { from, to }
    }

    /// The span a model wrote as words `from` to `to`, counted from 1. A 0 does not
    /// fit any message, so it stays out of range for the check to report.
    #[must_use]
    pub const fn from_shown(from: usize, to: usize) -> Self {
        Self::new(from.wrapping_sub(1), to.wrapping_sub(1))
    }

    /// The word numbers a model is shown, counted from 1.
    #[must_use]
    pub const fn shown(self) -> (usize, usize) {
        (self.from.wrapping_add(1), self.to.wrapping_add(1))
    }
}

/// How a span travels to and from a model: word numbers counted from 1.
#[derive(Serialize, Deserialize)]
struct ShownSpan {
    from: usize,
    to: usize,
}

impl Serialize for Span {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (from, to) = self.shown();
        ShownSpan { from, to }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let shown = ShownSpan::deserialize(deserializer)?;
        Ok(Self::from_shown(shown.from, shown.to))
    }
}

/// A span that does not fit the message it points into.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("words {from} to {to} are not in a message of {count} words")]
pub struct SpanError {
    /// First word asked for.
    pub from: usize,
    /// Last word asked for.
    pub to: usize,
    /// Words in the message.
    pub count: usize,
}

impl Words {
    /// Splits `text`.
    #[must_use]
    pub fn split(text: &str) -> Self {
        let mut words = Vec::new();
        let mut start = None;
        for (index, character) in text.char_indices() {
            match (character.is_whitespace(), start) {
                (true, Some(begin)) => {
                    words.push(Word {
                        text: text[begin..index].to_owned(),
                        start: begin,
                        end: index,
                    });
                    start = None;
                }
                (false, None) => start = Some(index),
                _ => {}
            }
        }
        if let Some(begin) = start {
            words.push(Word {
                text: text[begin..].to_owned(),
                start: begin,
                end: text.len(),
            });
        }
        Self {
            text: text.to_owned(),
            words,
        }
    }

    /// The message as given.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many words it has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Whether it has none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// The words.
    #[must_use]
    pub fn words(&self) -> &[Word] {
        &self.words
    }

    /// Checks that `span` is inside the message.
    ///
    /// # Errors
    ///
    /// [`SpanError`] when it is not.
    pub const fn check(&self, span: Span) -> Result<(), SpanError> {
        if span.from <= span.to && span.to < self.words.len() {
            Ok(())
        } else {
            Err(SpanError {
                from: span.from,
                to: span.to,
                count: self.words.len(),
            })
        }
    }

    /// The exact text a span covers, from its first word's start to its last word's end.
    ///
    /// # Errors
    ///
    /// [`SpanError`] when the span is not inside the message.
    pub fn slice(&self, span: Span) -> Result<&str, SpanError> {
        let (start, end) = self.bytes(span)?;
        Ok(&self.text[start..end])
    }

    /// The byte range a span covers.
    ///
    /// # Errors
    ///
    /// [`SpanError`] when the span is not inside the message.
    pub fn bytes(&self, span: Span) -> Result<(usize, usize), SpanError> {
        self.check(span)?;
        Ok((self.words[span.from].start, self.words[span.to].end))
    }

    /// The words inside `span` that `copied` repeats, compared without case and without
    /// the punctuation at either end of a word; `None` when they are not there.
    #[must_use]
    pub fn narrow(&self, span: Span, copied: &str) -> Option<Span> {
        self.places(span, copied)?.into_iter().next()
    }

    /// Where in the whole message `copied` is repeated, when it is repeated in one place only.
    #[must_use]
    pub fn only_place(&self, copied: &str) -> Option<Span> {
        let whole = Span::new(0, self.words.len().checked_sub(1)?);
        match self.places(whole, copied)?.as_slice() {
            [place] => Some(*place),
            _ => None,
        }
    }

    /// Every run of words inside `span` that repeats `copied`, first to last.
    fn places(&self, span: Span, copied: &str) -> Option<Vec<Span>> {
        let wanted: Vec<String> = copied.split_whitespace().map(bare).collect();
        let wanted: Vec<&String> = wanted.iter().filter(|word| !word.is_empty()).collect();
        self.check(span).ok()?;
        if wanted.is_empty() || wanted.len() > span.to - span.from + 1 {
            return None;
        }
        let inside: Vec<String> = self.words[span.from..=span.to]
            .iter()
            .map(|word| bare(&word.text))
            .collect();
        Some(
            (0..=inside.len() - wanted.len())
                .filter(|start| {
                    wanted
                        .iter()
                        .zip(&inside[*start..])
                        .all(|(want, have)| *want == have)
                })
                .map(|start| Span::new(span.from + start, span.from + start + wanted.len() - 1))
                .collect(),
        )
    }

    /// The words a span covers, with their byte range.
    ///
    /// # Errors
    ///
    /// [`SpanError`] when the span is not inside the message.
    pub fn range(&self, span: Span) -> Result<WordRange, SpanError> {
        let (start, end) = self.bytes(span)?;
        Ok(WordRange {
            first: span.from,
            last: span.to,
            start,
            end,
        })
    }

    /// The message rendered for a model: one line per sentence, each word prefixed
    /// with its number counted from 1, `S1: [1]I [2]want`.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut sentence = 1;
        let mut open = false;
        for (index, word) in self.words.iter().enumerate() {
            if !open {
                if sentence > 1 {
                    out.push('\n');
                }
                out.push_str(&format!("S{sentence}:"));
                open = true;
            }
            out.push_str(&format!(" [{}]{}", index + 1, word.text));
            if ends_sentence(&word.text) {
                sentence += 1;
                open = false;
            }
        }
        out
    }
}

/// Whether a word closes a sentence: it ends with `.`, `!`, `?` or `…`, possibly
/// followed by closing quotes or brackets.
fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', ')', ']', '»', '”', '’'])
        .ends_with(['.', '!', '?', '…'])
}

/// A word without case and without the punctuation at either end.
fn bare(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_keep_their_punctuation_and_their_byte_offsets() {
        let words = Words::split("  set the date, then «ciao»!  ");
        let texts: Vec<&str> = words.words().iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, vec!["set", "the", "date,", "then", "«ciao»!"]);
        assert_eq!(words.slice(Span::new(1, 2)).unwrap(), "the date,");
        assert_eq!(words.slice(Span::new(4, 4)).unwrap(), "«ciao»!");
        assert!(words.slice(Span::new(3, 5)).is_err());
        assert!(words.slice(Span::new(2, 1)).is_err());
    }

    #[test]
    fn rendering_numbers_words_and_breaks_sentences() {
        let words = Words::split("Add a bag. Then set the name? Thanks");
        assert_eq!(
            words.render(),
            "S1: [1]Add [2]a [3]bag.\nS2: [4]Then [5]set [6]the [7]name?\nS3: [8]Thanks"
        );
    }

    #[test]
    fn a_copy_has_one_place_only_when_the_message_repeats_it_once() {
        let words = Words::split("the Lisbon offsite, not the Porto one");
        assert_eq!(words.only_place("Lisbon offsite"), Some(Span::new(1, 2)));
        assert_eq!(words.only_place("the"), None);
        assert_eq!(words.only_place("Madrid"), None);
    }

    #[test]
    fn multi_byte_characters_slice_on_their_boundaries() {
        let words = Words::split("perché l'aereo è «puntuale»");
        assert_eq!(words.slice(Span::new(2, 3)).unwrap(), "è «puntuale»");
        let (start, end) = words.bytes(Span::new(0, 0)).unwrap();
        assert_eq!(&words.text()[start..end], "perché");
    }

    #[test]
    fn copied_words_narrow_a_pointer_and_never_move_it() {
        let words = Words::split("nome: Offsite Lisbona, partenza domani");
        let all = Span::new(0, 4);
        assert_eq!(words.narrow(all, "offsite lisbona"), Some(Span::new(1, 2)));
        assert_eq!(
            words.narrow(all, "«Offsite Lisbona»"),
            Some(Span::new(1, 2))
        );
        assert_eq!(
            words.narrow(Span::new(3, 4), "offsite"),
            None,
            "outside the pointer"
        );
        assert_eq!(
            words.narrow(all, "offsite roma"),
            None,
            "not the user's words"
        );
        assert_eq!(words.narrow(all, "  "), None);
    }

    #[test]
    fn a_model_counts_words_from_one() {
        let span: Span = serde_json::from_value(serde_json::json!({"from": 1, "to": 3})).unwrap();
        assert_eq!(span, Span::new(0, 2));
        assert_eq!(
            serde_json::to_value(span).unwrap(),
            serde_json::json!({"from": 1, "to": 3})
        );
        let zero: Span = serde_json::from_value(serde_json::json!({"from": 0, "to": 0})).unwrap();
        assert!(Words::split("one two").check(zero).is_err(), "0 is no word");
    }
}
