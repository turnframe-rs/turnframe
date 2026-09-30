//! A simulated user presses a card option by what it reads, its label, in any case: the
//! press reaches the card as the option's id, as a person's click would.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{SampleHarness, Scripted, narrating};
use turnframe_eval::runner::SampleIndex;
use turnframe_eval::simulate::{Goal, ScriptedUser, UserMove, converse};

const GOAL: &str = r#"
id = "trip.rebooked"
name = "The quoted flight is rebooked on its card"
want = "Confirm the rebooking on the card."
manners = ["terse"]
max_turns = 2

[[setup.cases]]
workflow = "trip"
case_id = "trip-1"
label = "Trip 1"
revision = 7

[setup.cases.state]
status = "awaiting_rebooking_confirmation"
name = "Lisbon, September"
travel_date = "2026-10-31"
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

[[reached.case_state]]
case_id = "trip-1"
path = "/status"
one_of = ["rebooking", "ticketed"]
"#;

#[tokio::test]
async fn a_press_names_a_card_option_by_its_label() {
    let goal: Goal = toml::from_str(GOAL).unwrap();
    let harness = SampleHarness::new(|_item, _sample, _turn| {
        Scripted::narrated_by(narrating().build_shared())
    });
    let user = ScriptedUser::new([UserMove::press("CONFIRM")]);

    let conversation = converse(&goal, "terse", &harness, &user, SampleIndex(0)).await;

    assert!(conversation.score.reached, "{}", conversation.transcript());
    assert_eq!(conversation.score.turns, 1);
}
