//! The workflow's per-phase guidance, and how far it travels.
//!
//! A briefing is the one thing understanding is shown that is *instructions*
//! rather than data: how to read a message about a case in the situation that
//! case is in. So it must arrive, per record and not per workflow, and nothing
//! about it is bounded unless a deployment bounds it.
//!
//! The trip workflow in the test kit briefs two of its phases and not the
//! rest, which is what makes "per phase" checkable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::flow::{BriefingBudget, StateField};
use turnframe_core::ids::TurnId;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{
    awaiting_rebooking_confirmation, incomplete_case, operations,
};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

const SETTING: &str = "Set the name to Lisbon";

/// The understanding of [`SETTING`], aimed at `inv-1`.
fn setting_the_name() -> turnframe_core::understanding::Understanding {
    UnderstandingBuilder::of(SETTING)
        .apply(
            operations::SET_NAME,
            token_for(turn_one(), "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            SETTING,
        )
        .build()
        .unwrap()
}

/// The briefing and stated fields of the one record the first turn showed
/// understanding, read from what it was given and not from what was built.
fn only_record(harness: &Harness) -> (Option<String>, Vec<StateField>) {
    let seen = harness.understander.seen();
    let shown = seen.first().expect("the turn was understood");
    let records: Vec<_> = shown
        .workflows
        .iter()
        .flat_map(|workflow| workflow.records.iter())
        .collect();
    assert_eq!(records.len(), 1, "this test seeds exactly one case");
    (records[0].briefing.clone(), records[0].fields.clone())
}

#[tokio::test]
async fn a_workflows_briefing_reaches_understanding() {
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(setting_the_name())
        .without_narration()
        .build()
        .await;

    harness
        .handle(harness.turn(turn_one(), SETTING))
        .await
        .unwrap();

    let briefing = only_record(&harness)
        .0
        .expect("a case being filled in carries the workflow's guidance");
    assert!(
        briefing.contains("A leg marked as kept is one the traveler asked to leave as it is"),
        "the workflow's own words travel verbatim: {briefing}"
    );
}

#[tokio::test]
async fn the_briefing_follows_the_phase_rather_than_the_workflow() {
    // The same workflow, the same words, a case in a different phase: a
    // per-workflow channel would carry the same guidance both times.
    let collecting = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(setting_the_name())
        .without_narration()
        .build()
        .await;
    collecting
        .handle(collecting.turn(turn_one(), SETTING))
        .await
        .unwrap();
    let while_collecting = only_record(&collecting)
        .0
        .expect("the collecting phase is briefed");

    let awaiting = Harness::builder()
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(setting_the_name())
        .without_narration()
        .build()
        .await;
    awaiting
        .handle(awaiting.turn(turn_one(), SETTING))
        .await
        .unwrap();
    let when_awaiting = only_record(&awaiting).0;

    assert!(
        while_collecting.contains("A leg marked as kept"),
        "{while_collecting}"
    );
    assert_ne!(
        Some(while_collecting),
        when_awaiting,
        "two phases of one workflow must be able to say different things"
    );
}

#[tokio::test]
async fn a_workflow_with_nothing_to_say_sends_no_briefing_at_all() {
    // `None` must not become an empty string: guidance that is always present
    // teaches the model to expect it and then hands it nothing.
    let text = "Tell me about this traveler";
    let harness = Harness::builder()
        .traveler(
            "trav-1",
            "Aurora",
            2,
            turnframe_test::workflows::traveler::TravelerState::default(),
        )
        .understands(UnderstandingBuilder::of(text).ask(text).build().unwrap())
        .without_narration()
        .build()
        .await;

    harness
        .handle(harness.turn(turn_one(), text))
        .await
        .unwrap();

    let (briefing, _) = only_record(&harness);
    assert!(
        briefing.is_none(),
        "a workflow that briefs nothing sends nothing: {briefing:?}"
    );
}

/// What ships: nothing is bounded, so a workflow's guidance arrives whole
/// however long it is.
#[test]
fn a_briefing_is_not_bounded_until_a_deployment_bounds_it() {
    let budget = BriefingBudget::conservative();
    assert_eq!(budget.max_bytes(), None);
    let corpus = "x".repeat(64 * 1024);
    assert_eq!(
        budget.apply(&corpus),
        corpus,
        "a fifty-kilobyte document is a corpus, not a verbose draft, and \
         cutting it to a number nobody chose is the library deciding for a \
         deployment it has not seen"
    );
}

/// A deployment that chose one gets exactly what it asked for, with no ceiling
/// above it: a limit an adopter cannot raise is not a configurable limit.
#[test]
fn a_chosen_budget_is_honoured_whatever_its_size() {
    let generous = BriefingBudget::new(50 * 1024);
    assert_eq!(generous.max_bytes(), Some(50 * 1024));

    let chosen = BriefingBudget::new(16 * 1024);
    let fits = "y".repeat(12 * 1024);
    assert_eq!(chosen.apply(&fits), fits);

    let long = "x".repeat(32 * 1024);
    let cut = chosen.apply(&long);
    assert!(cut.len() < long.len());
    assert!(
        cut.ends_with("… [briefing truncated]"),
        "a truncated briefing must not read as a whole one"
    );

    // And the cut lands on a character boundary rather than inside one.
    let accented = "à".repeat(16 * 1024);
    assert!(chosen.apply(&accented).starts_with('à'));

    // Round-tripping through configuration keeps the number and the absence.
    let parsed: BriefingBudget =
        serde_json::from_str(&(50 * 1024).to_string()).expect("a budget is a number");
    assert_eq!(parsed.max_bytes(), Some(50 * 1024));
    let none: BriefingBudget = serde_json::from_str("null").expect("or nothing at all");
    assert_eq!(none.max_bytes(), None);
}

#[tokio::test]
async fn a_raised_budget_carries_a_briefing_the_default_would_have_cut() {
    // The end-to-end half: the number on the config is what understanding sees.
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(setting_the_name())
        .without_narration()
        .config(
            turnframe_runtime::config::OrchestratorConfig::conservative().with_understanding(
                turnframe_runtime::config::UnderstandingConfig::conservative()
                    .with_briefing_budget(BriefingBudget::new(16 * 1024)),
            ),
        )
        .build()
        .await;

    harness
        .handle(harness.turn(turn_one(), SETTING))
        .await
        .unwrap();

    let briefing = only_record(&harness).0.expect("the case is briefed");
    assert!(
        !briefing.contains("[briefing truncated]"),
        "the workflow's guidance fits the budget this deployment chose"
    );
}

/// An act that reaches no case at all leaves a signal behind.
///
/// It leaves no target resolution, no policy decision and no command, so
/// without one the turn quietly does less than it said. A cancellation never
/// opens a record, so aiming it at a new one is refused before any case is
/// reached.
#[tokio::test]
async fn an_act_whose_target_resolves_to_nothing_is_reported() {
    let text = "Cancel that";
    let understanding = UnderstandingBuilder::of(text)
        .open(operations::WITHDRAW, "trip", serde_json::json!(null), text)
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;

    harness
        .handle(harness.turn(turn_one(), text))
        .await
        .unwrap();

    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "an act that resolves to nothing must not run"
    );
    assert_eq!(
        harness
            .observed()
            .count(turnframe_core::observe::Signal::TargetUnresolved),
        1,
        "the turn did less than it said, and something has to say so"
    );
}

/// Understanding is shown what the workflow says a case holds, not the record.
///
/// A record's own state can carry anything its workflow keeps, cards and
/// timestamps included. The test kit's trip declares the values a person gave
/// and deliberately declares neither its lifecycle status nor its reference
/// identifier, both of which its state carries.
#[tokio::test]
async fn understanding_is_shown_the_declared_state_and_not_the_record() {
    let text = "what is on this trip?";
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(UnderstandingBuilder::of(text).ask(text).build().unwrap())
        .without_narration()
        .build()
        .await;

    harness
        .handle(harness.turn(turn_one(), text))
        .await
        .unwrap();

    let (_, fields) = only_record(&harness);
    let names: Vec<&str> = fields.iter().map(|field| field.field.as_str()).collect();
    assert!(
        names.contains(&"name"),
        "what the user gave is there: {names:?}"
    );
    assert!(
        !names.contains(&"status") && !names.contains(&"external_status"),
        "and the lifecycle the workflow did not declare is not: {names:?}"
    );
}
