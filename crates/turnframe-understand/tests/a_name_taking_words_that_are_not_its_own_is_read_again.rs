//! A record named by the user's words is a copy of them: the verifier finding that it takes
//! words that are not the name sends it back, as it does any copied value. A record chosen
//! from those listed is that record, whatever words it was read from.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ArgumentValue, RecordValue};
use turnframe_understand::UnderstandingInput;

const SET_TRAVELER: &str = "trip.set_traveler";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SetTraveler {
    traveler: serde_json::Value,
}

#[tokio::test]
async fn a_name_taking_words_that_are_not_its_own_is_read_again() {
    let set_traveler = OperationSpec::new(SET_TRAVELER)
        .summary("Choose the traveler the trip is for.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<SetTraveler>()
        .argument("traveler", |a| {
            a.label("traveler").required().record("traveler")
        });
    let input = UnderstandingInput::new("the traveler is Omar Haddad", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler));
    // [1]the [2]traveler [3]is [4]Omar [5]Haddad
    let by_name = |name: &str, from: usize| {
        json!({"arguments": {"traveler": {
            "kind": "record", "name": name, "message": "current",
            "from": from, "to": 5, "record": "by_name"
        }}})
    };
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Names the traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_TRAVELER]}))
        .answer("u1/extract", by_name("traveler is Omar Haddad", 2))
        .answer(
            "u1/verify",
            json!({"reason": "Extra words.", "arguments": {"traveler": "too_much"}, "overall": "confirmed"}),
        )
        .answer("u1/extract.after_verify", by_name("Omar Haddad", 4))
        .answer(
            "u1/verify.after_repair",
            json!({"reason": "Named.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
        );
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::Named {
            workflow: "traveler".into(),
            named: "Omar Haddad".to_owned(),
        }),
        "{:?}",
        run.called()
    );
}
