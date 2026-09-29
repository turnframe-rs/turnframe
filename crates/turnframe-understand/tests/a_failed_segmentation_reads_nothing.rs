//! A message whose segmentation fails is unreadable: no act, no question, one reason.
mod support;

use support::{script, turn, understand};
use turnframe_core::understanding::Unreadable;

#[tokio::test]
async fn a_failed_segmentation_reads_nothing() {
    let run = understand(script(), &turn("set the name to Lisbon")).await;

    let understanding = &run.understanding;
    assert!(matches!(
        understanding.unreadable,
        Some(Unreadable::Segmentation { .. })
    ));
    assert!(understanding.acts.is_empty());
    assert!(understanding.questions.is_empty());
    assert_eq!(run.called(), vec!["turn/segment"]);
}
