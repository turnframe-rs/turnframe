//! Every complex item of the live corpus passes when its message is read correctly: its
//! expectations are reachable by the sample domains, so a live failure is the model's.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use serde_json::json;
use support::{SampleHarness, scripted, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::{ActTarget, ConstraintKind, RecordValue, Understanding};
use turnframe_eval::config::EvalConfig;
use turnframe_eval::corpus::{EvalItem, Suite};
use turnframe_eval::runner::Runner;
use turnframe_test::providers::UnderstandingBuilder;

fn price(euros: i64) -> serde_json::Value {
    json!({"minor": euros * 100, "currency": "EUR"})
}

fn extra(description: &str, quantity: u32, euros: i64) -> serde_json::Value {
    json!({"description": description, "quantity": quantity, "unit_price": price(euros)})
}

/// A correct reading of each complex item's message.
fn reading(item: &EvalItem, turn: TurnId) -> Understanding {
    let text = item.turn.text.as_deref().unwrap_or_default();
    let trip = |case: &str| token_for(turn, "trip", case);
    let traveler = |case: &str| token_for(turn, "traveler", case);
    let built = match item.id.0.as_str() {
        "complex.trip_in_full.en" => UnderstandingBuilder::of(text)
            .apply(
                "trip.add_extra",
                trip("trip-1"),
                extra("hotel", 3, 80),
                "3 nights at the hotel at 80 euros a night",
            )
            .apply(
                "trip.add_extra",
                trip("trip-1"),
                extra("lounge pass", 1, 15),
                "a lounge pass at 15 euros",
            )
            .apply(
                "trip.set_name",
                trip("trip-1"),
                json!({"value": "September offsite"}),
                "the name is September offsite",
            )
            .apply(
                "trip.set_travel_date",
                trip("trip-1"),
                json!({"value": "2023-12-31"}),
                "I would rather fly at the end of next month",
            ),
        "complex.new_traveler_for_a_trip.it" => {
            let opened = UnderstandingBuilder::of(text).open(
                "traveler.create_draft",
                "traveler",
                json!({"full_name": "Nadia Rinaldi"}),
                "Registra la viaggiatrice Nadia Rinaldi",
            );
            let created = opened.last_act().unwrap();
            opened
                .apply_to(
                    "traveler.change_email",
                    ActTarget::SameTurn { act: created },
                    json!({"value": "nadia@rinaldi.example"}),
                    "email nadia@rinaldi.example",
                )
                .apply(
                    "trip.set_traveler",
                    trip("trip-1"),
                    json!({}),
                    "mettila su questo viaggio",
                )
                .with_record("traveler", RecordValue::SameTurn { act: created })
                .apply(
                    "trip.add_extra",
                    trip("trip-1"),
                    extra("notte d'albergo", 2, 400),
                    "aggiungi due notti d'albergo a 400 euro l'una",
                )
        }
        "complex.rename_and_question.en" => UnderstandingBuilder::of(text)
            .apply(
                "traveler.set_full_name",
                traveler("trav-1"),
                json!({"value": "Marta Bianchi Ferri"}),
                "Change Marta Bianchi's name to Marta Bianchi Ferri",
            )
            .ask("what is still missing on the trip?"),
        "complex.correction_inside.en" => UnderstandingBuilder::of(text)
            .apply(
                "trip.add_extra",
                trip("trip-1"),
                extra("hotel night", 1, 135),
                "Add an extra for the hotel night at 150 euros, no wait, 135",
            )
            .apply(
                "trip.add_extra",
                trip("trip-1"),
                extra("airport transfer", 1, 40),
                "one for the airport transfer at 40 euros",
            )
            .constrain(ConstraintKind::DoNotSubmit, "don't rebook anything yet"),
        "complex.discursive.it" => UnderstandingBuilder::of(text)
            .apply(
                "trip.set_name",
                trip("trip-1"),
                json!({"value": "offsite ottobre"}),
                "metti come nome offsite ottobre",
            )
            .apply(
                "trip.set_travel_date",
                trip("trip-1"),
                json!({"value": "2023-11-30"}),
                "la data del viaggio al 30 novembre",
            )
            .apply(
                "traveler.set_loyalty_number",
                traveler("trav-1"),
                json!({"value": "AZ1234567"}),
                "il numero fedeltà della viaggiatrice è AZ1234567",
            ),
        "complex.two_trips.en" => UnderstandingBuilder::of(text)
            .apply(
                "trip.set_name",
                trip("trip-2"),
                json!({"value": "Porto"}),
                "On the Haddad trip set the name to Porto",
            )
            .apply(
                "trip.add_extra",
                trip("trip-1"),
                extra("hotel", 2, 60),
                "on the Bianchi one add 2 nights at the hotel at 60 euros",
            ),
        "complex.earlier_value.it" => UnderstandingBuilder::of(text)
            .apply(
                "trip.add_extra",
                trip("trip-1"),
                extra("notte d'albergo", 3, 50),
                "3 notti d'albergo a 50 euro a notte",
            )
            .apply(
                "trip.set_name",
                trip("trip-1"),
                json!({"value": "offsite di Lisbona"}),
                "come nome metti quello che ti ho detto prima",
            ),
        "complex.traveler_complete_and_question.it" => UnderstandingBuilder::of(text)
            .apply(
                "traveler.change_email",
                traveler("trav-1"),
                json!({"value": "nadia@rinaldi.example"}),
                "l'email è nadia@rinaldi.example",
            )
            .apply(
                "traveler.set_loyalty_number",
                traveler("trav-1"),
                json!({"value": "AZ7654321"}),
                "il numero fedeltà AZ7654321",
            )
            .ask("Di solito quanto bagaglio a mano è incluso nel biglietto?"),
        other => panic!("{other} has no reading here: add one"),
    };
    built.build().unwrap()
}

#[tokio::test]
async fn the_complex_items_pass_on_a_correct_reading() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/live_corpus");
    let suite = Suite::load_dir("live", dir).expect("the live corpus loads");
    let complex: Vec<&EvalItem> = suite
        .items
        .iter()
        .filter(|item| item.tags.iter().any(|tag| tag.0 == "complex"))
        .collect();
    assert_eq!(complex.len(), 8, "the complex section has eight items");
    let harness = SampleHarness::new(|item, _sample, turn| scripted(reading(item, turn)));
    let runner = Runner::new(EvalConfig::default());
    let mut failed = Vec::new();
    for item in complex {
        let report = runner.run_item(item, &harness).await;
        let failures = &report.samples[0].failures;
        if !failures.is_empty() {
            failed.push(format!("{}: {failures:?}", item.id.0));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
