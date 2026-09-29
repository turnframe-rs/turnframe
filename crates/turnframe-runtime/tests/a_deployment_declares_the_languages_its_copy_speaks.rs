//! A deployment declares the languages it serves, and the orchestrator refuses to start
//! while any sentence the server writes itself has no text in one of them. The built-in
//! copy speaks English and Italian; another language is added, or the Italian replaced,
//! field by field.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::Harness;
use turnframe_runtime::copy::ServerCopy;
use turnframe_runtime::orchestrator::BuildError;
use turnframe_runtime::policy::ConfirmationCopy;

#[tokio::test]
async fn the_built_in_copy_serves_english_and_italian() {
    let built = Harness::builder()
        .locales(&["en-GB", "it-IT"])
        .without_narration()
        .try_build()
        .await;
    assert!(built.is_ok());
}

#[tokio::test]
async fn a_language_the_copy_does_not_speak_stops_the_start() {
    let Err(refused) = Harness::builder()
        .locales(&["de-DE"])
        .without_narration()
        .try_build()
        .await
    else {
        panic!("a deployment in German with no German copy starts")
    };
    let BuildError::CopyMissing {
        locale, sentences, ..
    } = &refused
    else {
        panic!("{refused:?}")
    };
    assert_eq!(locale, "de-DE");
    assert!(!sentences.is_empty(), "{refused}");
}

#[tokio::test]
async fn a_language_added_field_by_field_is_served() {
    let german = ConfirmationCopy::standard().translated(
        "de",
        &[
            ("review_title", "Diese Änderungen prüfen"),
            ("confirm_title", "Diese Aktion bestätigen"),
            ("reauthenticate_title", "Ihre Identität bestätigen"),
            ("signature_title", "Dieses Dokument unterschreiben"),
            ("confirm_label", "Bestätigen"),
            ("decline_label", "Abbrechen"),
        ],
    );
    let deutsch = turnframe_core::locale::Locale::from("de-DE");
    assert!(turnframe_runtime::copy::missing(&german, &deutsch).is_empty());
    assert_eq!(
        german.confirm_label.resolve(&deutsch),
        "Bestätigen",
        "the new language is served"
    );
    assert_eq!(
        german.confirm_label.resolve(&"it-IT".into()),
        "Conferma",
        "and the built-in one is kept"
    );
}
