//! A conversation may hold a change that comes from outside it: the domain's own command,
//! applied between turns as its system would, never read from a message. The airline
//! re-quotes the fare, and the next turn finds the trip one revision on, at the new fare.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{SampleHarness, scripted};
use turnframe_eval::config::EvalConfig;
use turnframe_eval::corpus::{EvalItem, Suite};
use turnframe_eval::runner::Runner;
use turnframe_test::providers::UnderstandingBuilder;

const ITEM: &str = r#"
id = "outside.requote"
name = "An outside re-quote moves the trip one revision on"

[[before]]
external = { workflow = "trip", case_id = "trip-1", command = { requote = { leg = 1, flight = "AZ612", departs = "2026-10-05 13:10", fare_difference_cents = 13200 } } }

[turn]
text = "what now?"

[[setup.cases]]
workflow = "trip"
case_id = "trip-1"
label = "Trip 1"
revision = 7

[setup.cases.state]
status = "awaiting_rebooking_confirmation"
external_status = "awaiting_confirmation"

[setup.cases.state.traveler]
traveler_id = "5c1e0000-0000-4000-8000-000000000001"
display_name = "Marta Bianchi"

[[setup.cases.state.legs]]
number = 1
flight = "AZ610"
from = "FCO"
to = "LIS"
departs = "2026-10-05 07:40"
status = "cancelled"

[setup.cases.state.offer]
leg = 1
flight = "AZ612"
departs = "2026-10-05 13:10"
fare_difference_cents = 8400

[expect]
events = []

[[expect.case_revision]]
workflow = "trip"
case_id = "trip-1"
revision = 8

[[expect.case_state]]
case_id = "trip-1"
path = "/offer/fare_difference_cents"
equals = 13200
"#;

#[tokio::test]
async fn an_outside_change_between_turns_moves_the_record() {
    let item: EvalItem = toml::from_str(ITEM).unwrap();
    item.validate().unwrap();
    let harness = SampleHarness::new(|item, _sample, _turn| {
        scripted(
            UnderstandingBuilder::of(item.turn.text.as_deref().unwrap())
                .build()
                .unwrap(),
        )
    });

    let suite = Suite::new("outside", vec![item]).unwrap();
    let report = Runner::new(EvalConfig::default())
        .run(&suite, &harness)
        .await;

    assert!(
        (report.deterministic_pass_rate() - 1.0).abs() < 1e-9,
        "{}",
        report.summary()
    );
}
