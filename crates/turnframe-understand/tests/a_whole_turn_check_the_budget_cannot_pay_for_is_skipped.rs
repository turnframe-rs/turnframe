//! A whole-turn check the budget cannot pay for is skipped and says so; a repair it cannot
//! pay for holds the doubted act, which never runs on the reading it doubted.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand_in, words};
use turnframe_core::understanding::ActStatus;
use turnframe_tasks::Budget;
use turnframe_understand::{Settings, Step};

fn script_until_the_check() -> turnframe_tasks::testing::ScriptedTasks {
    script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
}

fn budget(calls: u32) -> Budget {
    let mut budget = Budget::understanding();
    budget.max_model_calls = Some(calls);
    budget
}

#[tokio::test]
async fn a_whole_turn_check_the_budget_cannot_pay_for_is_skipped() {
    // [1]name [2]Lisbon: segment, coverage, route, extract and verify spend five calls.
    let script = script_until_the_check().answer("turn/cross_check", json!({"findings": []}));
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand_in(script, &input, budget(5)).await;

    assert_eq!(run.understanding.acts[0].status, ActStatus::Ready);
    assert!(!run.was_called("turn/cross_check"));
    assert!(
        run.steps
            .steps()
            .iter()
            .any(|step| matches!(step, Step::CrossCheckSkipped { round: 1, .. })),
        "{:?}",
        run.steps.steps()
    );
}

#[tokio::test]
async fn a_repair_the_budget_cannot_pay_for_holds_the_act() {
    let script = script_until_the_check().answer(
        "turn/cross_check",
        json!({"findings": [{"kind": "wrong_value", "act": "u1.a1", "argument": "value",
                             "words": {"from": 2, "to": 2}}]}),
    );
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand_in(script, &input, budget(6)).await;

    let act = &run.understanding.acts[0];
    assert!(
        matches!(act.status, ActStatus::NeedsValue { .. }),
        "{act:?}"
    );
}
