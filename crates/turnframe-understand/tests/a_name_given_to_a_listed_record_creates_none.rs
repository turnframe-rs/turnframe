//! A part read both as creating a record and as giving a listed record of the same workflow
//! a value, both from the same words, gives that record the value: the creation is the
//! same words read twice, and nothing waits on it. A creation named by other words stands.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, script, today, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_understand::{RecordBrief, UnderstandingInput, WorkflowBrief};

const REGISTER: &str = "traveler.create_draft";
const RENAME: &str = "traveler.set_name";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Register {
    full_name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Rename {
    value: String,
}

fn input(message: &str) -> UnderstandingInput {
    let travelers = WorkflowBrief::new("traveler")
        .on_new_case(REGISTER)
        .operation(
            OperationSpec::new(REGISTER)
                .summary("Register a new traveler, with its name when given.")
                .target(TargetPolicy::NewCaseOnly)
                .mutating()
                .arguments::<Register>()
                .argument("full_name", |a| a.label("name").names_the_record()),
        )
        .operation(
            OperationSpec::new(RENAME)
                .summary("Set the traveler's registered name.")
                .target(TargetPolicy::RequiresExistingCase)
                .mutating()
                .arguments::<Rename>()
                .argument("value", |a| a.label("name").required()),
        )
        .record(RecordBrief::new("tok-traveler-1", "Bianchi", "collecting").offering([RENAME]));
    UnderstandingInput::new(message, "en-GB", today()).with_workflow(travelers)
}

fn one_request(from: usize, to: usize) -> serde_json::Value {
    json!({"analysis": "One request.", "units": [
        {"kind": "request", "words": {"from": from, "to": to}, "workflow": "traveler"}
    ]})
}

fn operations(run: &support::Run) -> Vec<String> {
    run.understanding
        .acts
        .iter()
        .filter_map(|act| act.operation().map(ToString::to_string))
        .collect()
}

#[tokio::test]
async fn a_name_given_to_a_listed_record_creates_none() {
    // [1]the [2]name [3]is [4]Beta
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", json!({"operations": [REGISTER, RENAME]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": words(4, 4)}}),
        )
        .answer("u1.a2/locate", json!({"record": "r1", "named": null}))
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"value": words(4, 4)}}),
        )
        .answer("u1/verify", confirmed(json!({"full_name": "stated"})))
        .answer("u1.a2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input("the name is Beta")).await;

    assert_eq!(
        operations(&run),
        vec![RENAME.to_owned()],
        "{:?} {:?}",
        run.called(),
        run.understanding
    );
}

#[tokio::test]
async fn a_creation_named_by_other_words_stands() {
    // [1]register [2]Beta, [3]and [4]the [5]name [6]is [7]Gamma
    let script = script()
        .answer("turn/segment", one_request(1, 7))
        .answer("u1/route", json!({"operations": [REGISTER, RENAME]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": words(2, 2)}}),
        )
        .answer("u1.a2/locate", json!({"record": "r1", "named": null}))
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"value": words(7, 7)}}),
        )
        .answer("u1/verify", confirmed(json!({"full_name": "stated"})))
        .answer("u1.a2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input("register Beta, and the name is Gamma")).await;

    assert_eq!(operations(&run).len(), 2, "{:?}", run.understanding);
}
