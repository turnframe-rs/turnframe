//! The case directory has the last word on every candidate, whatever brought
//! it into the turn (spec §12.3, §25.4).
//!
//! The account is the boundary the runtime enforces; an organization, a
//! workspace or a legal entity is enforced in the application's
//! [`CaseDirectory`] and nowhere else. So the directory is asked about every case
//! a turn can address beyond its own answer, including the case of a card the
//! conversation left open and the cases a `SelectTarget` card offers: a card
//! written in one workspace stops naming its record once the actor moves to
//! another. An unchanged scope costs no extra question, a directory that
//! implements nothing new admits nothing beyond its list, and a confirmation on
//! a record that does not exist yet stays answerable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use support::{Harness, account, narrating, token_for};
use turnframe_core::case::CaseKey;
use turnframe_core::error::{InteractionError, OrchestratorError, StoreError};
use turnframe_core::ids::{ConversationId, TurnId};
use turnframe_core::interaction::{
    InteractionKind, InteractionRejection, InteractionStatus, InteractionView,
};
use turnframe_core::response::{AssistantTurn, ResponseBlock};
use turnframe_core::turn::{ActorContext, TurnInput};
use turnframe_core::understanding::{ActTarget, ConstraintKind};
use turnframe_runtime::orchestrator::{CaseCandidate, CaseDirectory, StaticCaseDirectory};
use turnframe_runtime::policy::CONFIRM_OPTION_ID;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

// ---------------------------------------------------------------------------
// A directory with a scope narrower than the account.
// ---------------------------------------------------------------------------

/// Lists the cases of the workspace the actor is currently in.
///
/// It is the shape the threat model describes: one account, several workspaces,
/// and an actor who can move between them without changing account or
/// conversation. `candidates` is scoped, which is all an adopter has to get
/// right.
///
/// [`authorize_case`](CaseDirectory::authorize_case) is overridden only to
/// *record the question*; the answer it gives — `None` — is exactly the answer
/// the trait's own default gives, which the third test checks separately.
#[derive(Debug, Default)]
struct ScopedDirectory {
    by_workspace: HashMap<String, Vec<CaseCandidate>>,
    asked: Mutex<Vec<CaseKey>>,
}

impl ScopedDirectory {
    fn new() -> Self {
        Self::default()
    }

    /// Puts a case in a workspace, under the label that workspace shows.
    fn with_case(mut self, workspace: &str, workflow: &str, case_id: &str, label: &str) -> Self {
        self.by_workspace
            .entry(workspace.to_owned())
            .or_default()
            .push(CaseCandidate::new(CaseKey::new(workflow, case_id), label));
        self
    }

    /// The cases the directory was asked to authorize, in order.
    fn asked(&self) -> Vec<CaseKey> {
        self.asked
            .lock()
            .expect("the recorder is not poisoned")
            .clone()
    }
}

#[async_trait::async_trait]
impl CaseDirectory for ScopedDirectory {
    async fn candidates(
        &self,
        actor: &ActorContext,
        _conversation: &ConversationId,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        let workspace = workspace_of(actor);
        Ok(self
            .by_workspace
            .get(&workspace)
            .cloned()
            .unwrap_or_default())
    }

    async fn authorize_case(
        &self,
        _actor: &ActorContext,
        _conversation: &ConversationId,
        key: &CaseKey,
    ) -> Result<Option<CaseCandidate>, StoreError> {
        self.asked
            .lock()
            .expect("the recorder is not poisoned")
            .push(key.clone());
        Ok(None)
    }
}

/// The workspace an actor is in, as this application encodes it.
fn workspace_of(actor: &ActorContext) -> String {
    actor
        .attributes
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The same turn, taken by an actor who is in `workspace`.
fn in_workspace(mut input: TurnInput, workspace: &str) -> TurnInput {
    input.actor.attributes.insert(
        "workspace".to_owned(),
        serde_json::Value::String(workspace.to_owned()),
    );
    input
}

/// The card a turn put on the wire.
fn card_of(turn: &AssistantTurn) -> InteractionView {
    turn.blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Interaction(block) => Some(block.view.clone()),
            _ => None,
        })
        .expect("the turn carries a card")
}

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

const AMBIGUOUS: &str = "Set the name on the Ferri trip to Lisbon";

/// Two trips called Ferri in the `north` workspace, one in `south`, and a
/// turn understood to name either Ferri trip, so it cannot pick between them.
async fn selection_card_in_north(
    directory: Arc<ScopedDirectory>,
    later_narrations: usize,
) -> (Harness, InteractionView) {
    let first = turn(1);
    let understanding = UnderstandingBuilder::of(AMBIGUOUS)
        .apply_to(
            operations::SET_NAME,
            ActTarget::Ambiguous {
                candidates: vec![
                    token_for(first, "trip", "trip-1"),
                    token_for(first, "trip", "trip-2"),
                ],
            },
            serde_json::json!({"value": "Lisbon"}),
            AMBIGUOUS,
        )
        .build()
        .unwrap();
    let mut provider = narrating();
    for _ in 0..later_narrations {
        provider = provider.acknowledging("Right, that one it is.");
    }
    let provider = provider.build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .trip("trip-3", "Bianchi", 1, incomplete_case())
        .understands(understanding)
        .provider(provider)
        .case_directory(directory)
        .build()
        .await;

    let answer = harness
        .handle(in_workspace(harness.turn(first, AMBIGUOUS), "north"))
        .await
        .unwrap();
    let card = card_of(&answer);
    assert_eq!(card.kind, InteractionKind::SelectTarget);
    (harness, card)
}

/// The directory the two workspaces share.
fn two_workspaces() -> Arc<ScopedDirectory> {
    Arc::new(
        ScopedDirectory::new()
            .with_case("north", "trip", "trip-1", "Ferri")
            .with_case("north", "trip", "trip-2", "Ferri")
            .with_case("south", "trip", "trip-3", "Bianchi"),
    )
}

// ---------------------------------------------------------------------------
// 1. The hole: a card written in one workspace, answered from another.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_card_stops_naming_its_case_once_the_actor_leaves_the_workspace_it_was_written_in() {
    let directory = two_workspaces();
    // No narration after the first turn's: the second must not get far enough
    // to say anything.
    let (harness, card) = selection_card_in_north(Arc::clone(&directory), 0).await;

    // The actor moves to another workspace of the same account, in the same
    // conversation, and clicks the option that names the second trip.
    let chosen = token_for(turn(1), "trip", "trip-2");
    let click = in_workspace(harness.click(turn(2), card.id, chosen.as_str(), 1), "south");
    let refused = harness
        .handle(click)
        .await
        .expect_err("the click is refused");

    assert!(
        matches!(
            refused,
            OrchestratorError::Interaction(InteractionError::Rejected(
                InteractionRejection::NotFound
            ))
        ),
        "the card is answered as an identifier that never existed, not as a \
         permission failure that would confirm it is there: {refused:?}"
    );

    // The directory was asked, and its refusal is what stopped the turn.
    let asked = directory.asked();
    assert!(
        asked.contains(&CaseKey::new("trip", "trip-1")),
        "the case the card sits on went to the directory: {asked:?}"
    );
    assert!(
        asked.contains(&CaseKey::new("trip", "trip-2")),
        "so did the case the select-target option offers: {asked:?}"
    );

    // Nothing moved, on either trip.
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "the case the card sits on is untouched"
    );
    assert!(
        harness.events("trip", "trip-2").await.is_empty(),
        "and so is the one the option named"
    );
    assert_eq!(
        harness.trip_revision("trip-2").value(),
        1,
        "the record did not move"
    );
    let still_open = harness
        .stores
        .interaction(&account(), &card.id)
        .await
        .expect("the card is still stored");
    assert_eq!(
        still_open.status(),
        InteractionStatus::Active,
        "and the card is neither resolved nor consumed: back in its own \
         workspace it is still there to answer"
    );
}

// ---------------------------------------------------------------------------
// 2. The ordinary turn: nothing changed, so nothing changes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_same_card_is_answered_normally_when_the_workspace_has_not_changed() {
    let directory = two_workspaces();
    let (harness, card) = selection_card_in_north(Arc::clone(&directory), 1).await;

    let chosen = token_for(turn(1), "trip", "trip-2");
    let click = in_workspace(harness.click(turn(2), card.id, chosen.as_str(), 1), "north");
    harness.handle(click).await.unwrap();

    assert_eq!(
        harness.events("trip", "trip-2").await,
        vec!["trip.name_set"],
        "the change the card was guarding landed on the case the user picked"
    );
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "and on no other case (I8)"
    );
    assert!(
        directory.asked().is_empty(),
        "and the directory was never asked a second question: the cases the \
         card names were in the answer it already gave, so the new rule costs \
         this turn nothing: {:?}",
        directory.asked()
    );
}

// ---------------------------------------------------------------------------
// 3. The default: an adopter who writes nothing new is already covered.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_directory_that_implements_nothing_new_admits_nothing_beyond_its_list() {
    let directory = StaticCaseDirectory::new()
        .with_case(CaseCandidate::new(CaseKey::new("trip", "trip-1"), "Ferri"));
    let actor = ActorContext::new(account(), "u1");

    let listed = directory
        .authorize_case(
            &actor,
            &ConversationId::nil(),
            &CaseKey::new("trip", "trip-1"),
        )
        .await
        .unwrap();
    let unlisted = directory
        .authorize_case(
            &actor,
            &ConversationId::nil(),
            &CaseKey::new("trip", "trip-2"),
        )
        .await
        .unwrap();

    assert!(
        listed.is_none() && unlisted.is_none(),
        "the default refuses everything it is asked about, because it is only \
         ever asked about cases the candidate list did not name"
    );
}

// ---------------------------------------------------------------------------
// 4. The exemption: a record that does not exist yet.
// ---------------------------------------------------------------------------

const CREATE: &str = "Create a new trip, but ask me before you save it";

#[tokio::test]
async fn a_confirmation_on_a_record_that_does_not_exist_yet_stays_answerable() {
    // A directory that lists nothing at all and refuses everything it is asked
    // about: the strictest one an adopter can write.
    let directory = Arc::new(ScopedDirectory::new());
    let understanding = UnderstandingBuilder::of(CREATE)
        .start("trip", "Create a new trip")
        .constrain(
            ConstraintKind::AskBeforeApplying,
            "ask me before you save it",
        )
        .build()
        .unwrap();
    let provider = narrating()
        .acknowledging("Done, the draft is there.")
        .build_shared();
    let harness = Harness::builder()
        .understands(understanding)
        .provider(provider)
        .case_directory(Arc::clone(&directory) as Arc<dyn CaseDirectory>)
        .build()
        .await;

    let asked_first = harness
        .handle(in_workspace(harness.turn(turn(1), CREATE), "north"))
        .await
        .unwrap();
    let card = card_of(&asked_first);
    assert_eq!(card.kind, InteractionKind::ConfirmCommand);
    let case_id = card.case_ref.case_id.to_string();

    // The confirmation sits on a case the runtime minted and nobody has
    // created. The directory cannot list it and would refuse it, so if the
    // runtime asked, the card would be unanswerable for ever.
    let confirmed = harness
        .handle(in_workspace(
            harness.click(
                turn(2),
                card.id,
                CONFIRM_OPTION_ID,
                card.case_ref.expected_revision.value(),
            ),
            "north",
        ))
        .await
        .unwrap();

    assert!(
        !confirmed.blocks.is_empty(),
        "the confirmation produced a turn"
    );
    assert_eq!(
        harness.events("trip", &case_id).await,
        vec!["trip.opened"],
        "the record the card was there to create exists now"
    );
    assert!(
        directory.asked().is_empty(),
        "and the directory was never asked about a case with no state: there \
         is nothing to authorize and nothing to read: {:?}",
        directory.asked()
    );
}
