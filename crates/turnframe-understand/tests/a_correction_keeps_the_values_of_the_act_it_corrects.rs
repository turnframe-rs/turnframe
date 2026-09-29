//! Told «the traveler pays for the extra» was done, the user corrects «no, the company
//! pays»: the correction gives the payer and keeps the extra of the act it corrects. A
//! value the correction gives itself is its own, and an act done on another record, or
//! of another operation, lends it nothing.
mod support;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, script, today, trip, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ActStatus, ArgumentValue, UnderstoodArgument};
use turnframe_understand::{PendingAct, UnderstandingInput, WorkflowBrief};

const ASSIGN_PAYER: &str = "trip.assign_payer";

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code, reason = "only its schema is read")]
struct AssignPayer {
    /// The extra's number as the record lists it.
    extra: u32,
    /// Who pays.
    payer: String,
}

fn assign_payer() -> OperationSpec {
    OperationSpec::new(ASSIGN_PAYER)
        .summary("Say who pays for one extra.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<AssignPayer>()
        .argument("extra", |a| a.label("extra number").required())
        .argument("payer", |a| a.label("who pays").required())
}

fn done(operation: &str, record: &str) -> PendingAct {
    let json = |value| UnderstoodArgument {
        value: ArgumentValue::Json(value),
        excerpt: None,
    };
    PendingAct {
        operation: operation.into(),
        record: Some(record.into()),
        given: BTreeMap::from([
            ("extra".to_owned(), json(json!(1))),
            ("payer".to_owned(), json(json!("traveler"))),
        ]),
        missing: Vec::new(),
    }
}

/// «no, the company pays», after `done`, the correction reading only the payer.
async fn corrected(
    done: PendingAct,
    extra: serde_json::Value,
    verdicts: serde_json::Value,
) -> Vec<(ActStatus, serde_json::Value)> {
    // [1]no, [2]the [3]company [4]pays
    let workflow = WorkflowBrief::new("trip")
        .operation(assign_payer())
        .record(trip(1, "Bianchi").offering([ASSIGN_PAYER]));
    let input = UnderstandingInput::new("no, the company pays", "en-GB", today())
        .with_workflow(workflow)
        .with_done(done);
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A correction.", "units": [{"kind": "correction",
                "words": {"from": 1, "to": 4}, "workflow": "trip", "corrects": null}]}),
        )
        .answer("u1/route", json!({"operations": [ASSIGN_PAYER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"extra": extra, "payer": words(3, 3)}}),
        )
        .answer("u1/verify", confirmed(verdicts));
    let run = understand(script, &input).await;
    run.understanding
        .acts
        .iter()
        .map(|act| {
            let values: serde_json::Map<String, serde_json::Value> = act
                .arguments
                .iter()
                .filter_map(|(name, given)| match &given.value {
                    ArgumentValue::Json(value) => Some((name.clone(), value.clone())),
                    _ => None,
                })
                .collect();
            (act.status.clone(), serde_json::Value::Object(values))
        })
        .collect()
}

#[tokio::test]
async fn a_correction_keeps_the_values_of_the_act_it_corrects() {
    let acts = corrected(
        done(ASSIGN_PAYER, "tok-trip-1"),
        json!({"kind": "not_given"}),
        json!({"payer": "stated"}),
    )
    .await;
    assert_eq!(
        acts,
        [(ActStatus::Ready, json!({"extra": 1, "payer": "company"}))]
    );
}

#[tokio::test]
async fn a_value_the_correction_gives_is_its_own() {
    let given = json!({"kind": "value", "message": "current", "from": 1, "to": 1, "value": 2});
    let acts = corrected(
        done(ASSIGN_PAYER, "tok-trip-1"),
        given,
        json!({"extra": "stated", "payer": "stated"}),
    )
    .await;
    assert_eq!(
        acts,
        [(ActStatus::Ready, json!({"extra": 2, "payer": "company"}))]
    );
}

#[tokio::test]
async fn an_act_done_on_another_record_or_of_another_operation_lends_nothing() {
    for other in [
        done(ASSIGN_PAYER, "tok-trip-2"),
        done("trip.set_name", "tok-trip-1"),
    ] {
        let acts = corrected(
            other,
            json!({"kind": "not_given"}),
            json!({"payer": "stated"}),
        )
        .await;
        assert!(
            matches!(acts.as_slice(), [(ActStatus::NeedsValue { .. }, _)]),
            "{acts:?}"
        );
    }
}
