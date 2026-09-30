//! Deterministic target resolution (spec §12).
//!
//! A model never sees a record identifier: it chooses among opaque [`TargetToken`]s
//! this module issued for the cases the actor may address, and understanding hands
//! back the token, a new case, a record an earlier act of the turn creates, the card on
//! screen, or several candidates. Several is a selection card, never a guess: nothing
//! here reads a timestamp, a list position or a confidence (I8).
//!
//! | Understood target | How it resolves |
//! | --- | --- |
//! | `Record` | through the [`TargetTokenMap`]: unknown is `Unauthorized`, vanished `Missing`, moved `Stale` |
//! | `New` | only when the operation's [`TargetPolicy`] permits it, minted by a [`CaseIdFactory`] |
//! | `SameTurn` | the case the earlier act mints, compiled against the state it leaves |
//! | `Card` | the case of the card on screen |
//! | `Ambiguous` | a selection among the candidates |
//! | an origin (§12.4) | [`TargetResolver::resolve_origin`]: exactly the record the surface named |

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use indexmap::IndexMap;
use turnframe_core::case::CaseRef;
use turnframe_core::hash::derive_uuid;
use turnframe_core::ids::{
    AccountId, CaseId, CaseRevision, OriginToken, TargetToken, TurnId, WorkflowKey,
};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::reduce::ActiveInteractionSummary;
use turnframe_core::target::{TargetCandidate, TargetResolution, TargetTokenMap};
use turnframe_core::understanding::{ActAction, ActId, ActTarget, UnderstoodAct};

/// One case the actor may address this turn, with its server-authored label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedCase {
    /// The case and the revision it was loaded at.
    pub case_ref: CaseRef,
    /// Human label shown to the model and on cards.
    pub label: String,
    /// Whether the case is in view only because the actor may reach it; see
    /// [`CaseCandidate::subject_only_when_named`](crate::orchestrator::CaseCandidate::subject_only_when_named).
    pub subject_only_when_named: bool,
}

impl AuthorizedCase {
    /// A candidate with a label.
    #[must_use]
    pub fn new(case_ref: CaseRef, label: impl Into<String>) -> Self {
        Self {
            case_ref,
            label: label.into(),
            subject_only_when_named: false,
        }
    }

    /// Declares that the case belongs to another thread of work.
    #[must_use]
    pub const fn reachable_only(mut self) -> Self {
        self.subject_only_when_named = true;
        self
    }
}

/// Mints the identifier of a case that does not exist yet.
///
/// Injected so applications keep their identifier format. It must be a pure function
/// of its arguments: the identifier is in the plan hash (I20).
pub trait CaseIdFactory: Send + Sync {
    /// The identifier for the new case `act` of `turn_id` asks for.
    fn new_case_id(&self, workflow: &WorkflowKey, turn_id: &TurnId, act: ActId) -> CaseId;
}

/// Domain-separation prefix of [`DerivedCaseIdFactory`].
const NEW_CASE_DOMAIN: &str = "turnframe.new_case.v2";

/// Derives the identifier from the turn, the workflow and the act.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DerivedCaseIdFactory;

impl CaseIdFactory for DerivedCaseIdFactory {
    fn new_case_id(&self, workflow: &WorkflowKey, turn_id: &TurnId, act: ActId) -> CaseId {
        let uuid = derive_uuid(
            NEW_CASE_DOMAIN,
            &[&turn_id.to_string(), workflow.as_str(), &act.to_string()],
        );
        CaseId::from(format!("tf_{}", uuid.simple()))
    }
}

/// What resolving one act's target produced.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetOutcome {
    /// The target resolved to a core resolution.
    Resolved {
        /// The resolution.
        resolution: TargetResolution,
        /// `true` when this act opens the case.
        new_case: bool,
    },
    /// The record an earlier act of the turn opens.
    SameTurn {
        /// That case, at revision zero.
        case_ref: CaseRef,
    },
    /// The act addressed the card on screen and there is none.
    NoActiveInteraction,
    /// The target is not one the operation accepts (§21.2).
    PolicyMismatch {
        /// The policy that refused it.
        policy: TargetPolicy,
    },
    /// A start that resumes an open case, with more than one in view.
    SeveralOpenCases,
    /// The act names no record, and its operation needs one.
    NoTarget,
}

impl TargetOutcome {
    /// A plain resolution of an existing case.
    #[must_use]
    pub const fn resolved(resolution: TargetResolution) -> Self {
        Self::Resolved {
            resolution,
            new_case: false,
        }
    }

    /// The exactly resolved case, when there is one.
    #[must_use]
    pub fn exact(&self) -> Option<&CaseRef> {
        match self {
            Self::Resolved { resolution, .. } => resolution.exact(),
            Self::SameTurn { case_ref } => Some(case_ref),
            _ => None,
        }
    }

    /// The core resolution, when the target resolved at all.
    #[must_use]
    pub fn resolution(&self) -> Option<TargetResolution> {
        match self {
            Self::Resolved { resolution, .. } => Some(resolution.clone()),
            Self::SameTurn { case_ref } => Some(TargetResolution::Exact {
                case_ref: case_ref.clone(),
            }),
            _ => None,
        }
    }

    /// Returns `true` when this act opens the case.
    #[must_use]
    pub const fn is_new_case(&self) -> bool {
        matches!(self, Self::Resolved { new_case: true, .. })
    }

    /// Returns `true` when the case does not exist yet, opened by this act or an
    /// earlier one, so it compiles against the state the turn's acts leave, or none.
    #[must_use]
    pub const fn is_unborn(&self) -> bool {
        matches!(
            self,
            Self::Resolved { new_case: true, .. } | Self::SameTurn { .. }
        )
    }
}

/// Builds a [`TargetResolver`] for one turn.
#[derive(Clone)]
pub struct TargetResolverBuilder {
    account_id: AccountId,
    turn_id: TurnId,
    candidates: Vec<AuthorizedCase>,
    origins: IndexMap<OriginToken, CaseRef>,
    active_interaction: Option<ActiveInteractionSummary>,
    resuming: BTreeSet<WorkflowKey>,
    case_ids: Arc<dyn CaseIdFactory>,
}

impl std::fmt::Debug for TargetResolverBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetResolverBuilder")
            .field("account_id", &self.account_id)
            .field("turn_id", &self.turn_id)
            .field("candidates", &self.candidates.len())
            .finish_non_exhaustive()
    }
}

impl TargetResolverBuilder {
    /// Adds an authorized case; a token is issued for it when the resolver is built.
    #[must_use]
    pub fn candidate(mut self, candidate: AuthorizedCase) -> Self {
        self.candidates.push(candidate);
        self
    }

    /// Adds several authorized cases.
    #[must_use]
    pub fn candidates(mut self, candidates: impl IntoIterator<Item = AuthorizedCase>) -> Self {
        self.candidates.extend(candidates);
        self
    }

    /// Binds a server-validated origin token to the record the surface named (§12.4).
    #[must_use]
    pub fn origin(mut self, origin: OriginToken, candidate: AuthorizedCase) -> Self {
        self.origins.insert(origin, candidate.case_ref.clone());
        self.candidates.push(candidate);
        self
    }

    /// Declares the card on screen, which a `Card` target resolves through.
    #[must_use]
    pub fn active_interaction(mut self, summary: ActiveInteractionSummary) -> Self {
        self.active_interaction = Some(summary);
        self
    }

    /// Declares that starting `workflow` reaches the case it already has.
    #[must_use]
    pub fn resuming(mut self, workflow: WorkflowKey) -> Self {
        self.resuming.insert(workflow);
        self
    }

    /// Replaces the identifier factory used for new cases.
    #[must_use]
    pub fn case_id_factory(mut self, factory: Arc<dyn CaseIdFactory>) -> Self {
        self.case_ids = factory;
        self
    }

    /// Issues the tokens and freezes the candidate list.
    #[must_use]
    pub fn build(self) -> TargetResolver {
        let mut resolver = TargetResolver {
            tokens: TargetTokenMap::new(self.account_id.clone(), self.turn_id),
            account_id: self.account_id,
            turn_id: self.turn_id,
            candidates: IndexMap::new(),
            origins: self.origins,
            active_interaction: self.active_interaction,
            resuming: self.resuming,
            case_ids: self.case_ids,
        };
        for candidate in self.candidates {
            resolver.admit(candidate);
        }
        resolver
    }
}

/// Resolves the targets of one turn against the cases the actor may address.
#[derive(Clone)]
pub struct TargetResolver {
    account_id: AccountId,
    turn_id: TurnId,
    tokens: TargetTokenMap,
    candidates: IndexMap<TargetToken, AuthorizedCase>,
    origins: IndexMap<OriginToken, CaseRef>,
    active_interaction: Option<ActiveInteractionSummary>,
    resuming: BTreeSet<WorkflowKey>,
    case_ids: Arc<dyn CaseIdFactory>,
}

impl std::fmt::Debug for TargetResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetResolver")
            .field("account_id", &self.account_id)
            .field("turn_id", &self.turn_id)
            .field("tokens", &self.tokens.len())
            .finish_non_exhaustive()
    }
}

impl TargetResolver {
    /// Starts a builder for `account_id` and `turn_id`.
    #[must_use]
    pub fn builder(account_id: AccountId, turn_id: TurnId) -> TargetResolverBuilder {
        TargetResolverBuilder {
            account_id,
            turn_id,
            candidates: Vec::new(),
            origins: IndexMap::new(),
            active_interaction: None,
            resuming: BTreeSet::new(),
            case_ids: Arc::new(DerivedCaseIdFactory),
        }
    }

    /// Issues a token for a case found after the turn started: a record the user named
    /// that the application's directory looked up.
    pub fn admit(&mut self, candidate: AuthorizedCase) -> TargetToken {
        let token = self
            .tokens
            .issue(candidate.case_ref.clone(), candidate.label.clone());
        self.candidates.insert(token.clone(), candidate);
        token
    }

    /// The account every lookup is scoped to.
    #[must_use]
    pub const fn account_id(&self) -> &AccountId {
        &self.account_id
    }

    /// The turn the tokens were issued for.
    #[must_use]
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// The token map.
    #[must_use]
    pub const fn token_map(&self) -> &TargetTokenMap {
        &self.tokens
    }

    /// Every case in view, by token and label.
    #[must_use]
    pub fn catalog(&self) -> Vec<TargetCandidate> {
        self.tokens.candidates()
    }

    /// The cases in view of one workflow.
    #[must_use]
    pub fn catalog_for(&self, workflow: &WorkflowKey) -> Vec<TargetCandidate> {
        self.tokens.candidates_for(workflow)
    }

    /// The authorized case behind a token.
    #[must_use]
    pub fn candidate(&self, token: &TargetToken) -> Option<&AuthorizedCase> {
        self.candidates.get(token)
    }

    /// Every authorized case in view, in the order admitted.
    pub fn in_view(&self) -> impl Iterator<Item = &AuthorizedCase> {
        self.candidates.values()
    }

    /// The card on screen, when there is one.
    #[must_use]
    pub const fn active_interaction(&self) -> Option<&ActiveInteractionSummary> {
        self.active_interaction.as_ref()
    }

    /// Resolves a server-validated origin reference (§12.4): bound resolves exactly,
    /// unbound is `Unauthorized`, so a guessed token reveals nothing.
    #[must_use]
    pub fn resolve_origin(&self, origin: &OriginToken) -> TargetResolution {
        match self.origins.get(origin) {
            Some(case_ref) => TargetResolution::Exact {
                case_ref: case_ref.clone(),
            },
            None => TargetResolution::Unauthorized,
        }
    }

    /// The target token that stands for a bound origin, if any.
    #[must_use]
    pub fn origin_token(&self, origin: &OriginToken) -> Option<&TargetToken> {
        let case_ref = self.origins.get(origin)?;
        self.tokens.token_for(&case_ref.key())
    }

    /// Resolves one act's target. `spec` is its operation, absent for a start;
    /// `same_turn` maps the acts of this turn that open a case to the case they open.
    #[must_use]
    pub fn resolve(
        &self,
        act: &UnderstoodAct,
        spec: Option<&OperationSpec>,
        same_turn: &BTreeMap<ActId, CaseRef>,
    ) -> TargetOutcome {
        if let ActAction::Start { workflow } = &act.action {
            return self.start(workflow, act.id);
        }
        let policy = spec.map_or(TargetPolicy::RequiresExistingCase, |spec| {
            spec.target_policy
        });
        if !policy.permits(&act.target) {
            return TargetOutcome::PolicyMismatch { policy };
        }
        match &act.target {
            ActTarget::Record { token } => {
                TargetOutcome::resolved(self.tokens.resolve(&self.account_id, token))
            }
            ActTarget::New { workflow } => self.mint_case(workflow, act.id),
            ActTarget::SameTurn { act: earlier } => match same_turn.get(earlier) {
                Some(case_ref) => TargetOutcome::SameTurn {
                    case_ref: case_ref.clone(),
                },
                None => TargetOutcome::resolved(TargetResolution::Missing),
            },
            ActTarget::Card => match &self.active_interaction {
                Some(summary) => TargetOutcome::resolved(TargetResolution::Exact {
                    case_ref: summary.case_ref.clone(),
                }),
                None => TargetOutcome::NoActiveInteraction,
            },
            ActTarget::Ambiguous { candidates } => {
                let candidates: Vec<TargetCandidate> = candidates
                    .iter()
                    .filter(|token| self.tokens.resolve(&self.account_id, token).is_exact())
                    .filter_map(|token| self.candidate_view(token))
                    .collect();
                TargetOutcome::resolved(match <[TargetCandidate; 1]>::try_from(candidates) {
                    Ok([only]) => TargetResolution::Exact {
                        case_ref: only.case_ref,
                    },
                    Err(several) if several.is_empty() => TargetResolution::Missing,
                    Err(several) => TargetResolution::Ambiguous {
                        candidates: several,
                    },
                })
            }
            ActTarget::NotListed { .. } => TargetOutcome::resolved(TargetResolution::Missing),
            _ => TargetOutcome::NoTarget,
        }
    }

    /// Where a start lands: a new case, or the one open case of a workflow that
    /// resumes; several open cases are refused, since a start has no target to ask about.
    fn start(&self, workflow: &WorkflowKey, act: ActId) -> TargetOutcome {
        if !self.resuming.contains(workflow) {
            return self.mint_case(workflow, act);
        }
        let open: Vec<TargetResolution> = self
            .candidates
            .iter()
            .filter(|(_, candidate)| &candidate.case_ref.workflow == workflow)
            .map(|(token, _)| self.tokens.resolve(&self.account_id, token))
            .filter(TargetResolution::is_exact)
            .collect();
        match <[TargetResolution; 1]>::try_from(open) {
            Ok([only]) => TargetOutcome::resolved(only),
            Err(several) if several.is_empty() => self.mint_case(workflow, act),
            Err(_) => TargetOutcome::SeveralOpenCases,
        }
    }

    fn mint_case(&self, workflow: &WorkflowKey, act: ActId) -> TargetOutcome {
        let case_id = self.case_ids.new_case_id(workflow, &self.turn_id, act);
        TargetOutcome::Resolved {
            resolution: TargetResolution::Exact {
                case_ref: CaseRef::new(workflow.clone(), case_id, CaseRevision::ZERO),
            },
            new_case: true,
        }
    }

    fn candidate_view(&self, token: &TargetToken) -> Option<TargetCandidate> {
        let entry = self.tokens.get(token)?;
        Some(TargetCandidate {
            token: token.clone(),
            case_ref: entry.case_ref.clone(),
            label: entry.label.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::hash::Digest;
    use turnframe_core::ids::{InteractionId, OptionId};
    use turnframe_core::interaction::{InteractionKind, TextResolutionPolicy};
    use turnframe_core::understanding::{ActStatus, UnitId, WordRange};

    fn case(id: &str, revision: u64) -> CaseRef {
        CaseRef::new("trip", id, CaseRevision(revision))
    }

    fn resolver() -> TargetResolver {
        TargetResolver::builder(AccountId::from("acct"), TurnId::nil())
            .candidate(AuthorizedCase::new(case("trip-1", 3), "Trip 1"))
            .candidate(AuthorizedCase::new(case("trip-2", 5), "Trip 2"))
            .build()
    }

    fn act(target: ActTarget) -> UnderstoodAct {
        UnderstoodAct {
            id: ActId::new(UnitId(1), 1),
            action: ActAction::Apply {
                operation: "trip.set_name".into(),
            },
            target,
            arguments: BTreeMap::new(),
            words: WordRange {
                first: 0,
                last: 0,
                start: 0,
                end: 1,
            },
            depends_on: Vec::new(),
            status: ActStatus::Ready,
        }
    }

    fn spec(policy: TargetPolicy) -> OperationSpec {
        OperationSpec::new("trip.set_name")
            .summary("s")
            .target(policy)
    }

    fn token(resolver: &TargetResolver, id: &str, revision: u64) -> TargetToken {
        resolver
            .token_map()
            .token_for(&case(id, revision).key())
            .unwrap()
            .clone()
    }

    #[test]
    fn a_token_resolves_through_the_map_and_a_foreign_one_does_not() {
        let resolver = resolver();
        let known = act(ActTarget::Record {
            token: token(&resolver, "trip-1", 3),
        });
        let spec = spec(TargetPolicy::RequiresExistingCase);
        let none = BTreeMap::new();
        assert_eq!(
            resolver.resolve(&known, Some(&spec), &none).exact(),
            Some(&case("trip-1", 3))
        );
        let forged = act(ActTarget::Record {
            token: "t_forged".into(),
        });
        assert_eq!(
            resolver.resolve(&forged, Some(&spec), &none).resolution(),
            Some(TargetResolution::Unauthorized)
        );
    }

    #[test]
    fn a_new_case_needs_a_policy_that_allows_it_and_is_derived_from_the_act() {
        let resolver = resolver();
        let new = act(ActTarget::New {
            workflow: "trip".into(),
        });
        let none = BTreeMap::new();
        assert_eq!(
            resolver.resolve(&new, Some(&spec(TargetPolicy::RequiresExistingCase)), &none),
            TargetOutcome::PolicyMismatch {
                policy: TargetPolicy::RequiresExistingCase
            }
        );
        let first = resolver.resolve(&new, Some(&spec(TargetPolicy::NewCaseOnly)), &none);
        assert!(first.is_new_case());
        assert_eq!(
            first,
            resolver.resolve(&new, Some(&spec(TargetPolicy::NewCaseOnly)), &none),
            "the same act mints the same identifier (I20)"
        );
    }

    #[test]
    fn a_same_turn_target_is_the_case_the_earlier_act_opens() {
        let resolver = resolver();
        let opener = ActId::new(UnitId(1), 1);
        let dependent = act(ActTarget::SameTurn { act: opener });
        let minted = BTreeMap::from([(opener, CaseRef::new("trip", "tf_new", CaseRevision::ZERO))]);
        let outcome = resolver.resolve(
            &dependent,
            Some(&spec(TargetPolicy::RequiresExistingCase)),
            &minted,
        );
        assert!(outcome.is_unborn() && !outcome.is_new_case());
        assert_eq!(outcome.exact().map(|c| c.case_id.as_str()), Some("tf_new"));
        let orphan = resolver.resolve(
            &dependent,
            Some(&spec(TargetPolicy::RequiresExistingCase)),
            &BTreeMap::new(),
        );
        assert_eq!(orphan.resolution(), Some(TargetResolution::Missing));
    }

    #[test]
    fn several_candidates_are_a_question_and_one_is_an_answer() {
        let resolver = resolver();
        let spec = spec(TargetPolicy::RequiresExistingCase);
        let both = act(ActTarget::Ambiguous {
            candidates: vec![token(&resolver, "trip-1", 3), token(&resolver, "trip-2", 5)],
        });
        assert!(matches!(
            resolver.resolve(&both, Some(&spec), &BTreeMap::new()).resolution(),
            Some(TargetResolution::Ambiguous { candidates }) if candidates.len() == 2
        ));
        let one = act(ActTarget::Ambiguous {
            candidates: vec![token(&resolver, "trip-2", 5)],
        });
        assert_eq!(
            resolver
                .resolve(&one, Some(&spec), &BTreeMap::new())
                .exact(),
            Some(&case("trip-2", 5))
        );
    }

    #[test]
    fn the_card_target_resolves_to_the_card_on_screen() {
        let summary = ActiveInteractionSummary {
            interaction_id: InteractionId::nil(),
            case_ref: case("trip-2", 5),
            kind: InteractionKind::SingleSelect,
            blocking: true,
            option_ids: vec![OptionId::from("a")],
            text_resolution: TextResolutionPolicy::Never,
            confirms_risk: turnframe_core::command::RiskClass::ReversibleLowRisk,
            payload_hash: Digest::of_bytes(b"p"),
        };
        let spec = spec(TargetPolicy::ActiveInteractionOnly);
        let card = act(ActTarget::Card);
        assert_eq!(
            resolver().resolve(&card, Some(&spec), &BTreeMap::new()),
            TargetOutcome::NoActiveInteraction
        );
        let with_card = TargetResolver::builder(AccountId::from("acct"), TurnId::nil())
            .active_interaction(summary)
            .build();
        assert_eq!(
            with_card
                .resolve(&card, Some(&spec), &BTreeMap::new())
                .exact(),
            Some(&case("trip-2", 5))
        );
    }
}
