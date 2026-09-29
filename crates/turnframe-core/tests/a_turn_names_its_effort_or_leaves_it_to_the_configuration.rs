//! A turn may force its effort; one that does not leaves it to the configuration, and
//! says nothing about it on the wire.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::effort::Effort;
use turnframe_core::ids::{AccountId, ConversationId, TurnId};
use turnframe_core::locale::Locale;
use turnframe_core::turn::{ActorContext, TurnInput};

fn turn(effort: Option<Effort>) -> TurnInput {
    TurnInput {
        turn_id: TurnId::from(uuid::Uuid::from_u128(1)),
        conversation_id: ConversationId::nil(),
        actor: ActorContext::new(AccountId::from("aurora"), "u1"),
        text: Some("hello".to_owned()),
        interaction_response: None,
        attachments: Vec::new(),
        origin: None,
        locale: Locale::from("en-GB"),
        effort,
    }
}

#[test]
fn a_turn_names_its_effort_or_leaves_it_to_the_configuration() {
    let unset = serde_json::to_value(turn(None)).unwrap();
    assert!(unset.get("effort").is_none(), "{unset}");
    let read: TurnInput = serde_json::from_value(unset).unwrap();
    assert_eq!(read.effort, None);

    let forced = serde_json::to_value(turn(Some(Effort::High))).unwrap();
    assert_eq!(forced["effort"], "high");
    let read: TurnInput = serde_json::from_value(forced).unwrap();
    assert_eq!(read.effort, Some(Effort::High));

    assert_eq!(Effort::default(), Effort::Medium);
    assert_eq!("low".parse::<Effort>().unwrap(), Effort::Low);
    assert!("extreme".parse::<Effort>().is_err());
}
