//! A conversation held by a simulated user is scored by code: the goal is reached when the
//! stores hold its state, and every reply that ended on a way forward counts no dead end.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use serde_json::json;
use support::{SampleHarness, Scripted, token_for};
use turnframe_eval::runner::SampleIndex;
use turnframe_eval::simulate::{Ending, Goal, ScriptedUser, UserMove, converse, turn_at};
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::operations;

const GOAL: &str = r#"
id = "trip.named"
name = "A trip gets its name"
want = "Name the trip «Lisbon offsite»."
manners = ["terse"]
max_turns = 4

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
equals = "Lisbon offsite"
"#;

const SAID: &str = "call it Lisbon offsite";

#[tokio::test]
async fn a_simulated_conversation_is_scored_from_the_stores() {
    let goal: Goal = toml::from_str(GOAL).unwrap();
    goal.validate().unwrap();
    let harness = SampleHarness::new(|_item, _sample, first| {
        let understanding = UnderstandingBuilder::of(SAID)
            .apply(
                operations::SET_NAME,
                token_for(turn_at(first, 0), "trip", "trip-1"),
                json!({ "value": "Lisbon offsite" }),
                SAID,
            )
            .build()
            .unwrap();
        let provider = ScriptedProvider::builder("scripted", "model-1")
            .acknowledging("Named. What day would she rather fly?")
            .build_shared();
        Scripted::narrated_by(provider).understood_as(understanding)
    });
    let user = ScriptedUser::new([UserMove::say(SAID)]);

    let conversation = converse(&goal, "terse", &harness, &user, SampleIndex(0)).await;

    assert_eq!(
        conversation.ended,
        Ending::Done,
        "{}",
        conversation.transcript()
    );
    assert!(conversation.score.reached, "{}", conversation.transcript());
    assert_eq!(conversation.score.turns, 1);
    assert_eq!(
        conversation.score.violations(),
        0,
        "{}",
        conversation.transcript()
    );
    assert!(
        conversation.transcript().contains(SAID),
        "{}",
        conversation.transcript()
    );
}

#[tokio::test]
async fn a_reply_ending_on_its_ask_as_a_statement_is_no_dead_end() {
    let goal: Goal = toml::from_str(GOAL).unwrap();
    let harness = SampleHarness::new(|_item, _sample, first| {
        let understanding = UnderstandingBuilder::of(SAID)
            .apply(
                operations::SET_NAME,
                token_for(turn_at(first, 0), "trip", "trip-1"),
                json!({ "value": "Lisbon offsite" }),
                SAID,
            )
            .build()
            .unwrap();
        let provider = ScriptedProvider::builder("scripted", "model-1")
            .acknowledging("Named. Still needed: the travel date.")
            .build_shared();
        Scripted::narrated_by(provider).understood_as(understanding)
    });
    let user = ScriptedUser::new([UserMove::say(SAID)]);

    let conversation = converse(&goal, "terse", &harness, &user, SampleIndex(0)).await;

    assert_eq!(
        conversation.score.dead_ends,
        0,
        "{}",
        conversation.transcript()
    );
}
