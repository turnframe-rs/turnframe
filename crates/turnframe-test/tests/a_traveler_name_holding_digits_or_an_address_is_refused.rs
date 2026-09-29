//! A person's name holds no digit and no `@`: «AZ7654321» or «nadia@rinaldi.example»
//! given as a full name is refused with a sentence saying so, and the answer can be read
//! again as the field it is. A name with accents, apostrophes or hyphens is a name.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_test::workflows::traveler::{TravelerCommand, incomplete_draft, rejection, validate};

fn named(value: &str) -> Result<(), String> {
    let set = TravelerCommand::SetName {
        value: value.to_owned(),
    };
    let created = TravelerCommand::CreateNamedDraft {
        full_name: value.to_owned(),
    };
    let on_draft =
        validate(Some(&incomplete_draft()), &set).map_err(|refused| refused.code.to_string());
    let at_start = validate(None, &created).map_err(|refused| refused.code.to_string());
    assert_eq!(on_draft, at_start, "{value}");
    on_draft
}

#[test]
fn a_traveler_name_holding_digits_or_an_address_is_refused() {
    for value in ["AZ7654321", "nadia@rinaldi.example", "Nadia 2"] {
        assert_eq!(
            named(value),
            Err(rejection::NOT_A_NAME.to_owned()),
            "{value}"
        );
    }
}

#[test]
fn a_name_with_accents_apostrophes_or_hyphens_is_a_name() {
    for value in ["Nadia Rinaldi", "Niccolò D'Amico", "Anne-Marie Lefèvre"] {
        assert_eq!(named(value), Ok(()), "{value}");
    }
}
