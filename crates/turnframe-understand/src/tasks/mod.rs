//! The understanding task kinds. Each renders its context, states its answer schema with
//! the closed sets of this call, and checks answers structurally.
//!
//! | Task | Asks |
//! | --- | --- |
//! | [`segment`] | which units the message holds, and their words |
//! | [`coverage`] | which requests the units missed |
//! | [`cross_check`] | whether the whole reading says what the message says |
//! | [`respects`] | whether an act changes what the user asked to keep as it is |
//! | [`route`] | which operation a request asks for |
//! | [`locate`] | which record it is about |
//! | [`extract`] | the values of its arguments, as words and expressions |
//! | [`verify`] | whether those values are what the user said |
//! | [`question_frame`] | what a question is about |

pub mod coverage;
pub mod cross_check;
pub mod extract;
pub mod locate;
pub mod question_frame;
pub mod respects;
pub mod route;
pub mod segment;
pub mod take_up;
pub mod verify;

use turnframe_tasks::StructuralError;

use crate::words::{Span, Words};

/// A structural error for a pointer outside its message.
pub(crate) fn out_of_range(what: &str, span: Span, words: &Words) -> StructuralError {
    let (from, to) = span.shown();
    StructuralError::new(
        "span_out_of_range",
        format!(
            "{what}: words {from} to {to} are not in the message, whose words are numbered 1 to {}",
            words.len()
        ),
    )
}

/// Checks a pointer against its message.
pub(crate) fn check_span(what: &str, span: Span, words: &Words) -> Result<(), StructuralError> {
    words
        .check(span)
        .map_err(|_| out_of_range(what, span, words))
}

/// A structural error for a value outside its closed set.
pub(crate) fn not_one_of(field: &str, value: &str, allowed: &[String]) -> StructuralError {
    StructuralError::new(
        "not_in_set",
        format!(
            "`{field}` must be one of: {}; `{value}` is not",
            allowed.join(", ")
        ),
    )
}

/// Checks a value against its closed set.
pub(crate) fn check_one_of(
    field: &str,
    value: &str,
    allowed: &[String],
) -> Result<(), StructuralError> {
    if allowed.iter().any(|candidate| candidate == value) {
        Ok(())
    } else {
        Err(not_one_of(field, value, allowed))
    }
}
