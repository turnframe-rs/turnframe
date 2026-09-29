//! What the user is told when the domain says no.
//!
//! A refused act yields two things. The **notice** is deterministic and reaches
//! the user whether or not a model runs, so a turn that spent its budget still
//! says no. The **fact** is what stops the narrator's prose contradicting the
//! notice beside it. Without both, a user reasonably concludes the write went
//! through.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::locale::Locale;
use turnframe_core::observe::Signal;
use turnframe_core::response::ResponseBlock;
use turnframe_core::understanding::Understanding;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "Set the name to nothing at all";

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// An understanding whose one act the domain refuses, with copy of its own.
fn refused(turn: TurnId) -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "   "}),
            TEXT,
        )
        .build()
        .unwrap()
}

/// What the narrator was written from on its first call, or its `last`.
fn brief_of(provider: &ScriptedProvider, last: bool) -> String {
    let calls = provider.calls_for(ModelPurpose::Acknowledge);
    let call = if last { calls.last() } else { calls.first() }.expect("the narrator was called");
    call.user_text()
}

async fn run(
    locale: Locale,
    narrate: bool,
) -> (
    turnframe_core::response::AssistantTurn,
    Arc<support::RecordingObserver>,
    Arc<ScriptedProvider>,
) {
    let turn = turn_one();
    let mut builder = ScriptedProvider::builder("scripted", "model-1");
    if narrate {
        builder = builder.acknowledging("Noted.");
    }
    let provider = builder.build_shared();
    let mut harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(refused(turn))
        .provider(Arc::clone(&provider))
        .observing();
    if !narrate {
        harness = harness.without_narration();
    }
    let harness = harness.build().await;

    let mut input = harness.turn(turn, TEXT);
    input.locale = locale;
    let answered = harness.handle(input).await.unwrap();
    (answered, harness.observed(), provider)
}

#[tokio::test]
async fn the_user_is_told_the_act_was_refused_and_why() {
    let (answered, seen, _) = run(Locale::from("en-GB"), true).await;

    let notices: Vec<String> = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Notice(notice) => {
                Some(notice.text.resolve(&Locale::from("en-GB")).to_owned())
            }
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|text| text.contains("name cannot be empty")),
        "the domain's own sentence reaches the user: {notices:?}"
    );
    assert_eq!(
        seen.count(Signal::ActRefused),
        1,
        "and an operator can count it"
    );
}

#[tokio::test]
async fn the_refusal_is_reported_in_the_readers_language() {
    let (answered, _, _) = run(Locale::from("it-IT"), true).await;
    let notices: Vec<String> = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Notice(notice) => {
                Some(notice.text.resolve(&Locale::from("it-IT")).to_owned())
            }
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|text| text.contains("non può essere vuoto")),
        "a rejection's copy is localized like any other server copy: {notices:?}"
    );
}

#[tokio::test]
async fn the_refusal_reaches_the_user_even_with_no_narration() {
    let (answered, seen, _) = run(Locale::from("en-GB"), false).await;
    assert!(
        answered
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Notice(_))),
        "a refusal does not depend on a model running"
    );
    assert_eq!(seen.count(Signal::ActRefused), 1);
}

#[tokio::test]
async fn the_narrator_is_told_so_its_prose_does_not_contradict_the_notice() {
    let (_, _, provider) = run(Locale::from("en-GB"), true).await;
    let brief = brief_of(&provider, false);

    assert!(
        brief.contains("\"not_done\": [\n    \"A trip name cannot be empty."),
        "the narrator is shown the same sentence the notice carries: {brief}"
    );
}

#[tokio::test]
async fn nothing_is_written_and_nothing_claims_it_was() {
    let (answered, _, _) = run(Locale::from("en-GB"), true).await;
    assert!(
        !answered
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Receipt(_))),
        "a refused act produces no receipt, so nothing claims it happened"
    );
}

/// Declining a card the server drew is reported whatever the card carries: a
/// card with no deferred act behind it still owes the user a "no".
mod declining_a_card {
    use super::*;
    use turnframe_core::ids::OptionId;
    use turnframe_core::response::NoticeSeverity;
    use turnframe_core::turn::InteractionResponse;
    use turnframe_runtime::policy::DECLINE_OPTION_ID;
    use turnframe_test::workflows::trip::complete_case;

    const CANCEL: &str = "Cancel that trip";

    fn cancel(first: TurnId) -> Understanding {
        UnderstandingBuilder::of(CANCEL)
            .apply(
                operations::WITHDRAW,
                token_for(first, "trip", "trip-1"),
                serde_json::json!(null),
                CANCEL,
            )
            .build()
            .unwrap()
    }

    /// Raises a confirmation card by asking to cancel, then answers it with the
    /// declining option.
    async fn decline(
        provider: Arc<ScriptedProvider>,
        narrate: bool,
    ) -> turnframe_core::response::AssistantTurn {
        let first = TurnId::from(uuid::Uuid::from_u128(1));
        let second = TurnId::from(uuid::Uuid::from_u128(2));
        let mut builder = Harness::builder()
            .trip("trip-1", "Trip 1", 3, complete_case())
            .understands(cancel(first))
            .provider(provider);
        if !narrate {
            builder = builder.without_narration();
        }
        let harness = builder.build().await;

        let asked = harness.handle(harness.turn(first, CANCEL)).await.unwrap();
        let card = asked
            .blocks
            .iter()
            .find_map(|block| match block {
                ResponseBlock::Interaction(block) => Some(&block.view),
                _ => None,
            })
            .expect("a confirmation card is on screen");
        // The option's action is server-side and not in the view, so the
        // declining button is named by the id the engine gives it.
        let declining = OptionId::from(DECLINE_OPTION_ID);
        assert!(
            card.options.iter().any(|option| option.id == declining),
            "a confirmation card offers a way to say no"
        );

        let mut input = harness.turn(second, "");
        input.text = None;
        input.interaction_response = Some(InteractionResponse {
            interaction_id: card.id,
            option_id: declining,
            expected_case_revision: card.case_ref.expected_revision,
            freeform_input: None,
        });
        harness.handle(input).await.unwrap()
    }

    async fn decline_a_confirmation() -> turnframe_core::response::AssistantTurn {
        let provider = ScriptedProvider::builder("scripted", "model-1").build_shared();
        decline(provider, false).await
    }

    /// The same decline, with narration on, returning the brief the writer got.
    async fn decline_with_narration() -> String {
        let provider = ScriptedProvider::builder("scripted", "model-1")
            .acknowledging("Asking.")
            .acknowledging("All right, nothing done.")
            .build_shared();
        decline(Arc::clone(&provider), true).await;
        brief_of(&provider, true)
    }

    #[tokio::test]
    async fn saying_no_is_reported_even_with_nothing_deferred_behind_the_card() {
        let answered = decline_a_confirmation().await;
        let notices: Vec<(String, NoticeSeverity)> = answered
            .blocks
            .iter()
            .filter_map(|block| match block {
                ResponseBlock::Notice(notice) => Some((notice.code.clone(), notice.severity)),
                _ => None,
            })
            .collect();
        assert!(
            !notices.is_empty(),
            "a person pressed \"no\" on a card the server drew, and the turn has to say so"
        );
    }

    /// The prose has to be able to say the user declined, or it writes a
    /// generic offer of help above the notice.
    #[tokio::test]
    async fn the_narrator_is_told_the_user_declined() {
        let brief = decline_with_narration().await;
        assert!(
            brief.contains("The user answered «"),
            "the writer is told what the user did: {brief}"
        );
    }

    /// A decline is the user saying no; a refusal is the server saying no. One
    /// fact for each, because they are different sentences.
    #[tokio::test]
    async fn a_decline_is_not_reported_as_a_refusal() {
        let brief = decline_with_narration().await;
        assert!(brief.contains("The user answered «"), "{brief}");
        assert!(
            !brief.contains("could not be done"),
            "nothing was refused: the user declined, which reads differently: {brief}"
        );
    }

    #[tokio::test]
    async fn declining_runs_nothing() {
        let answered = decline_a_confirmation().await;
        assert!(
            !answered
                .blocks
                .iter()
                .any(|block| matches!(block, ResponseBlock::Receipt(_))),
            "refusing is refusing: no receipt, because nothing happened"
        );
    }
}

/// A target nothing could resolve is a refusal with no case behind it, and is
/// reported like one: a notice, a countable signal, and a fact naming no case.
mod an_unresolvable_target {
    use super::*;
    use turnframe_core::understanding::ActTarget;

    const NAMED: &str = "The second one";

    async fn run_unresolved() -> (
        turnframe_core::response::AssistantTurn,
        Arc<support::RecordingObserver>,
    ) {
        // A record named in words that name nothing in view.
        let mut understanding = UnderstandingBuilder::of(NAMED)
            .apply_to(
                operations::SET_NAME,
                ActTarget::NotListed {
                    workflow: "trip".into(),
                    words: None,
                },
                serde_json::json!({"value": NAMED}),
                NAMED,
            )
            .build()
            .unwrap();
        let words = understanding.acts[0].words;
        understanding.acts[0].target = ActTarget::NotListed {
            workflow: "trip".into(),
            words: Some(words),
        };
        let provider = ScriptedProvider::builder("scripted", "model-1")
            .acknowledging("That could not be done.")
            .build_shared();
        let harness = Harness::builder()
            .trip("trip-1", "Trip 1", 3, incomplete_case())
            .understands(understanding)
            .provider(Arc::clone(&provider))
            .observing()
            .build()
            .await;
        let answered = harness
            .handle(harness.turn(turn_one(), NAMED))
            .await
            .unwrap();
        (answered, harness.observed())
    }

    #[tokio::test]
    async fn the_user_is_told_something_could_not_be_done() {
        let (answered, seen) = run_unresolved().await;
        assert!(
            answered
                .blocks
                .iter()
                .any(|block| matches!(block, ResponseBlock::Notice(_))),
            "a turn that dropped the only act the user asked for has to say so"
        );
        assert_eq!(
            seen.count(Signal::ActRefused),
            1,
            "and it is countable beside the domain's own refusals"
        );
    }

    #[tokio::test]
    async fn the_refusal_names_no_case_because_none_was_reached() {
        let (answered, _) = run_unresolved().await;
        let facts: Vec<&turnframe_core::response::NarratableFact> = answered
            .blocks
            .iter()
            .filter_map(|block| match block {
                ResponseBlock::Transition(block) => Some(&block.facts_used),
                _ => None,
            })
            .flatten()
            .collect();
        let refused = facts
            .iter()
            .find(|fact| {
                matches!(
                    fact,
                    turnframe_core::response::NarratableFact::ActRefused { .. }
                )
            })
            .expect("the refusal is a fact the turn may rest on");
        let turnframe_core::response::NarratableFact::ActRefused { case_ref, code, .. } = refused
        else {
            unreachable!()
        };
        assert!(
            case_ref.is_none(),
            "the refusal is that no case was reached, so there is none to name"
        );
        assert!(
            code.contains("target") || code.contains("interaction"),
            "the code names why nothing resolved: {code}"
        );
    }
}
