//! Deterministic target resolution (spec §12).
//!
//! The model never sees raw case identifiers. The runtime issues opaque
//! [`TargetToken`]s per turn through a [`TargetTokenMap`] scoped to one account;
//! the map is the only authority on what a token means. Unknown tokens and
//! tokens of other tenants resolve identically to [`TargetResolution::Unauthorized`]
//! so a guessed token reveals nothing (spec §25.4).

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::case::{CaseKey, CaseRef};
use crate::command::CommandOrigin;
use crate::hash::{Digest, digest_hex};
use crate::ids::{AccountId, CaseRevision, OperationKey, TargetToken, TurnId, WorkflowKey};
use crate::understanding::ActId;

/// One authorized candidate offered to the model or to a selection card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetCandidate {
    /// Opaque token.
    pub token: TargetToken,
    /// The case behind it.
    pub case_ref: CaseRef,
    /// Server-authored human label (e.g. "Trip 12 - Ferri").
    pub label: String,
}

/// Outcome of resolving a proposed target (spec §12.2). Only `Exact` may reach
/// command compilation.
///
/// New outcomes are expected, so downstream matches need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TargetResolution {
    /// Exactly one authorized case.
    Exact {
        /// The case.
        case_ref: CaseRef,
    },
    /// Several authorized cases; never pick one (I8).
    Ambiguous {
        /// The candidates for a selection interaction.
        candidates: Vec<TargetCandidate>,
    },
    /// The token was issued this turn but the case no longer exists.
    Missing,
    /// Unknown token or another tenant's token.
    Unauthorized,
    /// The case moved past the revision the token was issued at.
    Stale {
        /// Reference at issue time.
        case_ref: CaseRef,
        /// Revision now.
        current_revision: CaseRevision,
    },
}

impl TargetResolution {
    /// Returns the case when the resolution is exact.
    #[must_use]
    pub fn exact(&self) -> Option<&CaseRef> {
        match self {
            Self::Exact { case_ref } => Some(case_ref),
            _ => None,
        }
    }

    /// Returns `true` for `Exact`.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::Exact { .. })
    }
}

/// What kind of act was resolved.
///
/// It grows with [`ActAction`](crate::understanding::ActAction), so downstream
/// matches need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResolvedActKind {
    /// Apply a catalog operation.
    ApplyOperation {
        /// The operation.
        operation: OperationKey,
    },
    /// Start a new case (the case reference carries revision zero).
    StartWorkflow,
}

/// An understood act after exact target resolution, the input of
/// [`crate::flow::WorkflowDefinition::compile_act`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAct {
    /// The act.
    pub act: ActId,
    /// What the act does.
    pub kind: ResolvedActKind,
    /// The exact case at the loaded revision.
    pub case_ref: CaseRef,
    /// Arguments, parsed and schema-validated by the reducer.
    pub arguments: serde_json::Value,
    /// Digest of the words and choices the act rests on.
    pub evidence_digest: Digest,
}

impl ResolvedAct {
    /// The operation named by the act, when any.
    #[must_use]
    pub fn operation(&self) -> Option<&OperationKey> {
        match &self.kind {
            ResolvedActKind::ApplyOperation { operation } => Some(operation),
            ResolvedActKind::StartWorkflow => None,
        }
    }

    /// The origin a low-risk command compiled from this act carries.
    #[must_use]
    pub fn direct_origin(&self) -> CommandOrigin {
        CommandOrigin::DirectSafeUserAct {
            evidence_digest: self.evidence_digest.clone(),
        }
    }
}

/// Server-side state of an issued token.
///
/// New states are expected, so downstream matches need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TokenStatus {
    /// Resolves to the case.
    Authorized,
    /// The case vanished after the token was issued.
    Missing,
    /// The case moved to another revision after the token was issued.
    Stale {
        /// Revision now.
        current_revision: CaseRevision,
    },
}

/// What a token maps to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetEntry {
    /// The case at issue time.
    pub case_ref: CaseRef,
    /// Human label.
    pub label: String,
    /// Current status.
    pub status: TokenStatus,
}

const TOKEN_DOMAIN: &str = "turnframe.target.v1";
const TOKEN_PREFIX: &str = "t_";
const TOKEN_MIN_HEX: usize = 12;

/// Account-scoped map from opaque tokens to authorized cases for one turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetTokenMap {
    account_id: AccountId,
    turn_id: TurnId,
    entries: IndexMap<TargetToken, TargetEntry>,
}

impl TargetTokenMap {
    /// Creates an empty map for one account and turn.
    #[must_use]
    pub fn new(account_id: AccountId, turn_id: TurnId) -> Self {
        Self {
            account_id,
            turn_id,
            entries: IndexMap::new(),
        }
    }

    /// The account the map is scoped to.
    #[must_use]
    pub fn account_id(&self) -> &AccountId {
        &self.account_id
    }

    /// The turn the tokens were issued for.
    #[must_use]
    pub fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Issues (or returns the existing) token for a case.
    ///
    /// Tokens are derived deterministically from the account, turn and case so a
    /// replayed turn reproduces the same catalog; they are opaque and carry no
    /// database identifier. Re-issuing for a case already in the map updates the
    /// label and revision and restores `Authorized`.
    pub fn issue(&mut self, case_ref: CaseRef, label: impl Into<String>) -> TargetToken {
        let key = case_ref.key();
        if let Some((token, entry)) = self
            .entries
            .iter_mut()
            .find(|(_, e)| e.case_ref.key() == key)
        {
            entry.case_ref = case_ref;
            entry.label = label.into();
            entry.status = TokenStatus::Authorized;
            return token.clone();
        }
        let token = self.fresh_token(&key);
        self.entries.insert(
            token.clone(),
            TargetEntry {
                case_ref,
                label: label.into(),
                status: TokenStatus::Authorized,
            },
        );
        token
    }

    fn fresh_token(&self, key: &CaseKey) -> TargetToken {
        let material = format!(
            "{TOKEN_DOMAIN}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            self.account_id, self.turn_id, key.workflow, key.case_id
        );
        let hex = digest_hex(material.as_bytes());
        let mut len = TOKEN_MIN_HEX;
        loop {
            let candidate = TargetToken::from(format!("{TOKEN_PREFIX}{}", &hex[..len]));
            if !self.entries.contains_key(&candidate) || len >= hex.len() {
                return candidate;
            }
            len = (len + 4).min(hex.len());
        }
    }

    /// Resolves a token for `account`.
    ///
    /// A different account, an unknown token and another tenant's token all
    /// yield `Unauthorized`. `Missing` and `Stale` are reported only for tokens
    /// issued in this map and marked as such.
    #[must_use]
    pub fn resolve(&self, account: &AccountId, token: &TargetToken) -> TargetResolution {
        if account != &self.account_id {
            return TargetResolution::Unauthorized;
        }
        match self.entries.get(token) {
            None => TargetResolution::Unauthorized,
            Some(entry) => match &entry.status {
                TokenStatus::Authorized => TargetResolution::Exact {
                    case_ref: entry.case_ref.clone(),
                },
                TokenStatus::Missing => TargetResolution::Missing,
                TokenStatus::Stale { current_revision } => TargetResolution::Stale {
                    case_ref: entry.case_ref.clone(),
                    current_revision: *current_revision,
                },
            },
        }
    }

    /// Marks a token's case as gone. Returns `false` when the token is unknown.
    pub fn mark_missing(&mut self, token: &TargetToken) -> bool {
        match self.entries.get_mut(token) {
            Some(entry) => {
                entry.status = TokenStatus::Missing;
                true
            }
            None => false,
        }
    }

    /// Marks a token's case as moved to `current_revision`. Returns `false` when
    /// the token is unknown.
    pub fn mark_stale(&mut self, token: &TargetToken, current_revision: CaseRevision) -> bool {
        match self.entries.get_mut(token) {
            Some(entry) => {
                entry.status = TokenStatus::Stale { current_revision };
                true
            }
            None => false,
        }
    }

    /// The token issued for a case, if any.
    #[must_use]
    pub fn token_for(&self, key: &CaseKey) -> Option<&TargetToken> {
        self.entries
            .iter()
            .find(|(_, e)| e.case_ref.key() == *key)
            .map(|(token, _)| token)
    }

    /// Entry behind a token, without an account check. For the runtime that owns
    /// the map; client-facing paths must use [`Self::resolve`].
    #[must_use]
    pub fn get(&self, token: &TargetToken) -> Option<&TargetEntry> {
        self.entries.get(token)
    }

    /// All authorized candidates, for the interpretation catalog.
    #[must_use]
    pub fn candidates(&self) -> Vec<TargetCandidate> {
        self.entries
            .iter()
            .filter(|(_, e)| e.status == TokenStatus::Authorized)
            .map(|(token, e)| TargetCandidate {
                token: token.clone(),
                case_ref: e.case_ref.clone(),
                label: e.label.clone(),
            })
            .collect()
    }

    /// Authorized candidates of one workflow, for resolving mentions.
    #[must_use]
    pub fn candidates_for(&self, workflow: &WorkflowKey) -> Vec<TargetCandidate> {
        self.candidates()
            .into_iter()
            .filter(|c| &c.case_ref.workflow == workflow)
            .collect()
    }

    /// Iterates all entries.
    pub fn iter(&self) -> impl Iterator<Item = (&TargetToken, &TargetEntry)> {
        self.entries.iter()
    }

    /// Number of tokens.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no token was issued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> TargetTokenMap {
        TargetTokenMap::new(AccountId::from("acct"), TurnId::nil())
    }

    #[test]
    fn issue_is_deterministic_and_opaque() {
        let mut a = map();
        let mut b = map();
        let case = CaseRef::new("trip", "trip-42", CaseRevision(3));
        let t1 = a.issue(case.clone(), "Trip 42");
        let t2 = b.issue(case.clone(), "Trip 42");
        assert_eq!(t1, t2);
        assert!(t1.as_str().starts_with("t_"));
        assert!(!t1.as_str().contains("trip-42"));
        assert_eq!(a.issue(case.with_revision(CaseRevision(4)), "Trip 42"), t1);
        assert_eq!(a.len(), 1);
        assert_eq!(
            a.get(&t1).unwrap().case_ref.expected_revision,
            CaseRevision(4)
        );
    }

    #[test]
    fn tokens_are_bound_to_their_account_and_turn() {
        let case = CaseRef::new("trip", "trip-42", CaseRevision(3));
        let mut mine = map();
        let token = mine.issue(case.clone(), "Trip 42");
        let mut other_account = TargetTokenMap::new(AccountId::from("other"), TurnId::nil());
        let mut other_turn = TargetTokenMap::new(AccountId::from("acct"), TurnId::new());
        assert_ne!(
            token,
            other_account.issue(case.clone(), "Trip 42"),
            "another tenant never gets the same token for the same case"
        );
        assert_ne!(
            token,
            other_turn.issue(case, "Trip 42"),
            "a token issued in another turn is a different token"
        );
        // A token from one map means nothing in another.
        assert_eq!(
            other_turn.resolve(&AccountId::from("acct"), &token),
            TargetResolution::Unauthorized
        );
    }

    #[test]
    fn unknown_and_foreign_tokens_are_indistinguishable() {
        let mut m = map();
        let token = m.issue(CaseRef::new("trip", "i1", CaseRevision(1)), "l");
        assert_eq!(
            m.resolve(&AccountId::from("other"), &token),
            TargetResolution::Unauthorized
        );
        assert_eq!(
            m.resolve(
                &AccountId::from("acct"),
                &TargetToken::from("t_deadbeef0000")
            ),
            TargetResolution::Unauthorized
        );
        assert!(m.resolve(&AccountId::from("acct"), &token).is_exact());
    }

    #[test]
    fn missing_and_stale_are_kept_for_issued_tokens() {
        let mut m = map();
        let token = m.issue(CaseRef::new("trip", "i1", CaseRevision(1)), "l");
        assert!(m.mark_stale(&token, CaseRevision(2)));
        assert_eq!(
            m.resolve(&AccountId::from("acct"), &token),
            TargetResolution::Stale {
                case_ref: CaseRef::new("trip", "i1", CaseRevision(1)),
                current_revision: CaseRevision(2)
            }
        );
        assert!(m.mark_missing(&token));
        assert_eq!(
            m.resolve(&AccountId::from("acct"), &token),
            TargetResolution::Missing
        );
        assert!(!m.mark_missing(&TargetToken::from("nope")));
        assert!(m.candidates().is_empty());
    }

    #[test]
    fn candidates_filter_by_workflow() {
        let mut m = map();
        m.issue(CaseRef::new("trip", "i1", CaseRevision(1)), "a");
        m.issue(CaseRef::new("traveler", "c1", CaseRevision(1)), "b");
        assert_eq!(m.candidates().len(), 2);
        assert_eq!(m.candidates_for(&WorkflowKey::from("trip")).len(), 1);
        assert!(m.token_for(&CaseKey::new("traveler", "c1")).is_some());
    }
}
