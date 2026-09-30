//! A goal written for a year is held on a day of that year: the harness set to an instant
//! gives every turn that day as today, so a user who names a date without its year is read
//! in the goal's year and not in the fixed instant the corpus runs at.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate};
use support::{SampleHarness, Scripted};
use turnframe_eval::runner::SampleIndex;
use turnframe_eval::simulate::{Goal, ScriptedUser, UserMove, converse};
use turnframe_test::providers::{ScriptedProvider, ScriptedUnderstanding};

const GOAL: &str = r#"
id = "trip.dated"
name = "A trip gets its travel date"
want = "Set the day she would rather fly to 21 October 2026."
manners = ["terse"]
max_turns = 1

[[setup.cases]]
workflow = "trip"
case_id = "trip-1"
label = "Trip 1"
revision = 3

[setup.cases.state]
status = "draft"

[[reached.case_state]]
case_id = "trip-1"
path = "/travel_date"
equals = "2026-10-21"
"#;

#[tokio::test]
async fn a_simulated_conversation_runs_on_the_day_its_harness_is_set_to() {
    let goal: Goal = toml::from_str(GOAL).unwrap();
    let seen: Arc<Mutex<Option<Arc<ScriptedUnderstanding>>>> = Arc::default();
    let kept = Arc::clone(&seen);
    let harness = SampleHarness::new(move |_item, _sample, _first| {
        let provider = ScriptedProvider::builder("scripted", "model-1")
            .acknowledging("Which day would she rather fly?")
            .build_shared();
        let scripted = Scripted::narrated_by(provider);
        *kept.lock().unwrap() = Some(Arc::clone(&scripted.understander));
        scripted
    })
    .at(DateTime::parse_from_rfc3339("2026-09-30T09:00:00Z")
        .unwrap()
        .into());
    let user = ScriptedUser::new([UserMove::say("on 21 October")]);

    converse(&goal, "terse", &harness, &user, SampleIndex(0)).await;

    let understander = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the world was prepared");
    let shown = understander.seen();
    assert_eq!(
        shown.first().map(|input| input.today),
        NaiveDate::from_ymd_opt(2026, 9, 30)
    );
}
