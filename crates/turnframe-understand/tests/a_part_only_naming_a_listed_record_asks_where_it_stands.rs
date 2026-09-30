//! A part that asks nothing of a record but names a listed one by its whole label, «open Trip
//! 1», is no misreading: it asks where that record stands. It is never reported as not
//! understood, and holds nothing else the message asks of that record.
#![allow(clippy::panic)]

mod support;

use serde_json::json;
use support::{script, turn, understand};
use turnframe_core::understanding::{QuestionTopic, UnitKind};

#[tokio::test]
async fn a_part_only_naming_a_listed_record_asks_where_it_stands() {
    // [1]open [2]Trip [3]1.
    let script = script()
        .answer("turn/segment", support::one_request(1, 3))
        .answer("u1/route", json!({"operations": ["none"]}))
        .answer("u1/frame", json!({"topic": "record_state", "record": "r1"}));
    let run = understand(script, &turn("open Trip 1.")).await;

    let understanding = &run.understanding;
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
    assert_eq!(
        understanding.units[0].kind,
        UnitKind::Question,
        "{understanding:?}"
    );
    let [question] = understanding.questions.as_slice() else {
        panic!("one question: {understanding:?}");
    };
    assert_eq!(
        question.record.as_ref().map(|t| t.as_str()),
        Some("tok-trip-1")
    );
}

#[tokio::test]
async fn the_record_it_names_is_the_questions_when_the_frame_names_none() {
    // [1]open [2]Trip [3]1.
    let script = script()
        .answer("turn/segment", support::one_request(1, 3))
        .answer("u1/route", json!({"operations": ["none"]}))
        .answer(
            "u1/frame",
            json!({"topic": "record_state", "record": "none"}),
        );
    let run = understand(script, &turn("open Trip 1.")).await;

    let understanding = &run.understanding;
    let [question] = understanding.questions.as_slice() else {
        panic!("one question: {understanding:?}");
    };
    assert_eq!(
        question.record.as_ref().map(|t| t.as_str()),
        Some("tok-trip-1")
    );
}

#[tokio::test]
async fn it_asks_where_the_record_stands_whatever_topic_the_frame_read() {
    // [1]open [2]Trip [3]1.
    let script = script()
        .answer("turn/segment", support::one_request(1, 3))
        .answer("u1/route", json!({"operations": ["none"]}))
        .answer("u1/frame", json!({"topic": "capabilities", "record": "r1"}));
    let run = understand(script, &turn("open Trip 1.")).await;

    let understanding = &run.understanding;
    let [question] = understanding.questions.as_slice() else {
        panic!("one question: {understanding:?}");
    };
    assert_eq!(question.topic, QuestionTopic::RecordState);
}

#[tokio::test]
async fn a_frame_reading_whether_it_can_be_done_does_not_route_it_again() {
    // [1]open [2]Trip [3]1.
    let open = json!({"operations": ["trip.open"]});
    let script = script()
        .answer("turn/segment", support::one_request(1, 3))
        .answer("u1/route", json!({"operations": ["none"]}))
        .answer("u1/frame", json!({"topic": "ability", "record": "r1"}))
        .answer("u1/route", open.clone())
        .answer("u1/route.again", open);
    let run = understand(script, &turn("open Trip 1.")).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    let [question] = understanding.questions.as_slice() else {
        panic!("one question: {understanding:?}");
    };
    assert_eq!(
        question.record.as_ref().map(|t| t.as_str()),
        Some("tok-trip-1")
    );
}
