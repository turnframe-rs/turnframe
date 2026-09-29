//! Every showcase item of the live corpus passes when its message is read correctly, so
//! a live failure of one of the four guarantees is the model's reading, never the item's.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use serde_json::json;
use support::{SampleHarness, Scripted, scripted, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::{ActTarget, ConstraintKind, RecordValue, Understanding};
use turnframe_eval::config::EvalConfig;
use turnframe_eval::corpus::{EvalItem, Suite};
use turnframe_eval::runner::Runner;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};

/// A correct reading of each showcase item's message; `None` for a click.
fn reading(item: &EvalItem, turn: TurnId) -> Option<Understanding> {
    let text = item.turn.text.as_deref()?;
    let trip = token_for(turn, "trip", "trip-1");
    let built = match item.id.0.as_str() {
        "showcase.protected_leg.en" => UnderstandingBuilder::of(text)
            .apply(
                "trip.request_rebooking",
                trip.clone(),
                json!({"leg": 1}),
                "Rebook the outbound on the flight the airline offered",
            )
            .apply(
                "trip.protect_leg",
                trip,
                json!({"leg": 2}),
                "don't touch the return",
            )
            .constrain(ConstraintKind::DoNotSubmit, "don't confirm anything yet"),
        "showcase.protected_leg.it" => UnderstandingBuilder::of(text)
            .apply(
                "trip.request_rebooking",
                trip.clone(),
                json!({"leg": 1}),
                "Cambia il volo d'andata con quello proposto dalla compagnia",
            )
            .apply(
                "trip.protect_leg",
                trip,
                json!({"leg": 2}),
                "non toccare il ritorno",
            )
            .constrain(ConstraintKind::DoNotSubmit, "non confermare ancora niente"),
        "showcase.protected_leg_misread.en" => UnderstandingBuilder::of(text)
            .apply(
                "trip.request_rebooking",
                trip,
                json!({"leg": 1}),
                "rebook the flight out",
            )
            .constrain(ConstraintKind::KeepUnchanged, "keep the one back as it is"),
        "showcase.dependent_acts.en" | "showcase.dependent_acts.it" => {
            let (register, put, add, bag) = if item.id.0.ends_with(".en") {
                (
                    "Register Nadia Rinaldi",
                    "put her on this trip",
                    "add a checked bag for her at 40 euros",
                    "checked bag",
                )
            } else {
                (
                    "Registra Nadia Rinaldi",
                    "mettila su questo viaggio",
                    "aggiungile un bagaglio da stiva a 40 euro",
                    "bagaglio da stiva",
                )
            };
            let opened = UnderstandingBuilder::of(text).open(
                "traveler.create_draft",
                "traveler",
                json!({"full_name": "Nadia Rinaldi"}),
                register,
            );
            let created = opened.last_act().unwrap();
            opened
                .apply("trip.set_traveler", trip.clone(), json!({}), put)
                .with_record("traveler", RecordValue::SameTurn { act: created })
                .apply_to(
                    "trip.add_extra",
                    ActTarget::Record { token: trip },
                    json!({"description": bag, "quantity": 1,
                           "unit_price": {"minor": 4_000, "currency": "EUR"}}),
                    add,
                )
        }
        other => panic!("{other} has no reading here: add one"),
    };
    Some(built.build().unwrap())
}

#[tokio::test]
async fn the_showcase_items_pass_on_a_correct_reading() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/live_corpus");
    let suite = Suite::load_dir("live", dir).expect("the live corpus loads");
    let showcase: Vec<&EvalItem> = suite
        .items
        .iter()
        .filter(|item| item.tags.iter().any(|tag| tag.0 == "showcase"))
        .collect();
    assert_eq!(showcase.len(), 8, "the showcase section has eight items");
    let harness = SampleHarness::new(|item, _sample, turn| match reading(item, turn) {
        Some(understanding) => scripted(understanding),
        None => Scripted::narrated_by(std::sync::Arc::new(
            ScriptedProvider::builder("scripted", "model-1")
                .acknowledging("Sent to the airline; it has not answered yet.")
                .build(),
        )),
    });
    let runner = Runner::new(EvalConfig::default());
    let mut failed = Vec::new();
    for item in showcase {
        let report = runner.run_item(item, &harness).await;
        let failures = &report.samples[0].failures;
        if !failures.is_empty() || report.samples[0].harness_error.is_some() {
            failed.push(format!(
                "{}: {failures:?} {:?}",
                item.id.0, report.samples[0].harness_error
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
