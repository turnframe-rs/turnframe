//! Server copy: the sentences the runtime writes itself, which a user reads. Each copy
//! struct ships English and Italian; a deployment declares the languages it serves with
//! [`OrchestratorBuilder::locales`](crate::orchestrator::OrchestratorBuilder::locales), and
//! building fails while any sentence has no text in one of them.

use turnframe_core::locale::{Locale, LocalizedText};

/// Copy a user reads, sentence by sentence, by the name of its field.
pub trait ServerCopy {
    /// The copy's name, for a report.
    fn name(&self) -> &'static str;
    /// Every sentence, by the name of its field.
    fn entries(&self) -> Vec<(&'static str, &LocalizedText)>;
    /// Every sentence, to change in place.
    fn entries_mut(&mut self) -> Vec<(&'static str, &mut LocalizedText)>;

    /// Lays `texts`, by field name, over this copy as its text in `locale`: a new
    /// language, or the built-in one replaced. A field left out keeps what it had.
    #[must_use]
    fn translated(mut self, locale: impl Into<Locale>, texts: &[(&str, &str)]) -> Self
    where
        Self: Sized,
    {
        let locale = locale.into();
        for (name, text) in self.entries_mut() {
            if let Some((_, translation)) = texts.iter().find(|(field, _)| *field == name) {
                text.translations
                    .insert(locale.clone(), (*translation).to_owned());
            }
        }
        self
    }
}

/// The sentences of `copy` with no text in the language of `locale`. The default text is
/// English, so English is always served.
#[must_use]
pub fn missing(copy: &dyn ServerCopy, locale: &Locale) -> Vec<&'static str> {
    if locale.same_language(&Locale::from("en")) {
        return Vec::new();
    }
    copy.entries()
        .into_iter()
        .filter(|(_, text)| {
            !text
                .translations
                .keys()
                .any(|translated| translated.same_language(locale))
        })
        .map(|(name, _)| name)
        .collect()
}

/// Implements [`ServerCopy`] over every field of a copy struct; a field left out does not
/// compile, so no sentence escapes the check.
macro_rules! server_copy {
    ($ty:ty, [$($field:ident),* $(,)?]) => {
        impl $crate::copy::ServerCopy for $ty {
            fn name(&self) -> &'static str {
                stringify!($ty)
            }

            fn entries(&self) -> Vec<(&'static str, &turnframe_core::locale::LocalizedText)> {
                let Self { $($field),* } = self;
                vec![$((stringify!($field), $field)),*]
            }

            fn entries_mut(
                &mut self,
            ) -> Vec<(&'static str, &mut turnframe_core::locale::LocalizedText)> {
                let Self { $($field),* } = self;
                vec![$((stringify!($field), $field)),*]
            }
        }
    };
}
pub(crate) use server_copy;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::AttachmentCopy;
    use crate::compose::{AskCopy, CompositionCopy};
    use crate::policy::ConfirmationCopy;
    use crate::reduce::NoticeCopy;

    fn built_in() -> Vec<Box<dyn ServerCopy>> {
        vec![
            Box::new(NoticeCopy::default()),
            Box::new(CompositionCopy::default()),
            Box::new(AskCopy::default()),
            Box::new(ConfirmationCopy::default()),
            Box::new(AttachmentCopy::default()),
        ]
    }

    fn placeholders(text: &str) -> Vec<&str> {
        let mut found: Vec<&str> = text
            .match_indices('{')
            .filter_map(|(at, _)| text[at..].find('}').map(|end| &text[at..=at + end]))
            .collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn every_built_in_sentence_speaks_italian_with_the_same_placeholders() {
        let italian = Locale::from("it-IT");
        for copy in built_in() {
            assert!(
                missing(copy.as_ref(), &italian).is_empty(),
                "{}",
                copy.name()
            );
            for (name, text) in copy.entries() {
                assert_eq!(
                    placeholders(text.resolve(&italian)),
                    placeholders(&text.default),
                    "{}.{name}",
                    copy.name()
                );
            }
        }
    }

    #[test]
    fn a_language_no_sentence_speaks_is_reported_sentence_by_sentence() {
        let german = Locale::from("de-DE");
        let copy = AskCopy::default();
        assert_eq!(missing(&copy, &german).len(), copy.entries().len());
        assert!(missing(&copy, &Locale::from("en-GB")).is_empty());
    }
}
