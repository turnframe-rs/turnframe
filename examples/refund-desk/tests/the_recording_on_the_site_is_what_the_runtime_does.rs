//! The recording the website plays is what the runtime does now: recorded twice it is the
//! same, and it is the file the site reads. After a change in behaviour, record it again
//! with `cargo run -p refund-desk -- --record website/src/data/refund-runs.json`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;

#[tokio::test]
async fn the_recording_on_the_site_is_what_the_runtime_does() {
    let first = runs::json(&runs::all().await.unwrap()).unwrap();
    let second = runs::json(&runs::all().await.unwrap()).unwrap();
    assert_eq!(first, second, "a recording is the same every time");
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../website/src/data/refund-runs.json"
    );
    let committed = std::fs::read_to_string(path).expect("the site's recording exists");
    assert!(
        committed == first,
        "the site's recording is out of date: run cargo run -p refund-desk -- --record website/src/data/refund-runs.json"
    );
}
