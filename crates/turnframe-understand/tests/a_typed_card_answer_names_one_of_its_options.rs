//! «yes, send it» on a card is an answer naming one of the card's own options.
mod support;

use serde_json::json;
use support::{script, turn, understand};
use turnframe_understand::OpenCard;

#[tokio::test]
async fn a_typed_card_answer_names_one_of_its_options() {
    let card = OpenCard::new("trip", "Send Trip 1 to Bianchi?")
        .about("tok-trip-1")
        .option("send", "Send")
        .option("keep", "Not yet");
    let script = script().answer(
        "turn/segment",
        json!({"analysis": "Answers the card.", "units": [
            {"kind": "card_answer", "words": {"from": 1, "to": 3}, "option": "send"}
        ]}),
    );
    let input = turn("yes, send it").with_card(card);
    let run = understand(script, &input).await;

    let answer = run
        .understanding
        .card_answer
        .as_ref()
        .expect("a card answer");
    assert_eq!(answer.option.as_str(), "send");
    assert!(run.understanding.acts.is_empty());
    let schema = &run.provider.calls()[0];
    let text = serde_json::to_string(&schema.output).unwrap();
    assert!(
        text.contains(r#""enum":["send","keep"]"#),
        "options are a closed set: {text}"
    );
}

#[tokio::test]
async fn a_click_only_card_offers_no_typed_answer() {
    let card = OpenCard::new("trip", "Send Trip 1?")
        .option("send", "Send")
        .click_only();
    let script = script().answer(
        "turn/segment",
        json!({"analysis": "Chitchat.", "units": [{"kind": "chitchat", "words": {"from": 1, "to": 1}}]}),
    );
    let run = understand(script, &turn("ok").with_card(card)).await;

    let text = serde_json::to_string(&run.provider.calls()[0].output).unwrap();
    assert!(
        !text.contains("card_answer"),
        "a click-only card takes no typed answer"
    );
}
