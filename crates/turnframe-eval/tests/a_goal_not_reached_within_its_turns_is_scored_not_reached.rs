//! A simulated user who does not reach the goal within its turns is scored not reached,
//! with every turn kept for the transcript; the run itself does not fail.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use serde_json::json;
use support::{SampleHarness, Scripted, token_for};
use turnframe_eval::simulate::{Ending, Goal, ScriptedUser, Simulation, UserMove, turn_at};
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::operations;

const GOAL: &str = r#"
id = "trip.named_porto"
name = "A trip is named Porto"
want = "Name the trip «Porto»."
manners = ["changes the subject"]
max_turns = 2

[[setup.cases]]
workflow = "trip"
case_id = "trip-1"
label = "Trip 1"
revision = 3

[setup.cases.state]
status = "draft"

[setup.cases.state.traveler]
traveler_id = "5c1e0000-0000-4000-8000-000000000001"
display_name = "Marta Bianchi"

[[reached.case_state]]
case_id = "trip-1"
path = "/name"
equals = "Porto"
"#;

const NAME: &str = "call it Lisbon offsite";
const DATE: &str = "she flies on 21 October";

#[tokio::test]
async fn a_goal_not_reached_within_its_turns_is_scored_not_reached() {
    let goal: Goal = toml::from_str(GOAL).unwrap();
    goal.validate().unwrap();
    let harness = SampleHarness::new(|_item, _sample, first| {
        let apply = |number, operation, value: serde_json::Value, text| {
            UnderstandingBuilder::of(text)
                .apply(
                    operation,
                    token_for(turn_at(first, number), "trip", "trip-1"),
                    value,
                    text,
                )
                .build()
                .unwrap()
        };
        let provider = ScriptedProvider::builder("scripted", "model-1")
            .acknowledging("Named. What day would she rather fly?")
            .acknowledging("Noted. Anything else?")
            .build_shared();
        Scripted::narrated_by(provider)
            .understood_as(apply(
                0,
                operations::SET_NAME,
                json!({"value": "Lisbon offsite"}),
                NAME,
            ))
            .understood_as(apply(
                1,
                operations::SET_TRAVEL_DATE,
                json!({"value": "2026-10-21"}),
                DATE,
            ))
    });
    let user = ScriptedUser::new([
        UserMove::say(NAME),
        UserMove::say(DATE),
        UserMove::say("one more thing"),
    ]);

    let report = Simulation::new().run(&[goal], &harness, &user).await;

    let [conversation] = report.conversations.as_slice() else {
        panic!("one conversation: {report:?}");
    };
    assert_eq!(conversation.ended, Ending::TurnLimit);
    assert!(!conversation.score.reached);
    assert_eq!(
        conversation.exchanges.len(),
        2,
        "{}",
        conversation.transcript()
    );
    let transcript = conversation.transcript();
    assert!(
        transcript.contains(NAME) && transcript.contains(DATE),
        "{transcript}"
    );
    assert!(
        report.summary().contains("reached: 0/1"),
        "{}",
        report.summary()
    );
}
