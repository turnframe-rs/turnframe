//! A shared vocabulary for what two turn paths disagreed about.
//!
//! # Why a type and not a report format
//!
//! Classifying divergences is where the value of a shadow stage is. An adopter
//! who runs [`TurnPlanner`](crate::planning::TurnPlanner) beside their existing
//! tool loop ends up with two descriptions of the same turn and a pile of
//! differences, and what they need next is a name for each kind of difference
//! — the same name the next adopter uses, and the same name the next release
//! uses, so a report can be read across both.
//!
//! Each side describes its turn as a [`TurnSummary`]. The Turnframe side gets
//! one for free from [`PlannedTurn::summary`](crate::planning::PlannedTurn::summary);
//! the authoritative side is filled in by the adopter from their own logs, and
//! the fields are deliberately shallow enough that a free tool-calling agent
//! can be described in them.
//!
//! # The asymmetry, in the type
//!
//! Not every difference is a regression, and a comparison that cannot say so
//! will be read as though it were. This module states the case
//! that matters: when this library **refuses a mutation because the target was
//! ambiguous** and the previous path performed it anyway, the finding is
//! against the previous path. It picked one of several records the user might
//! have meant, and being right most of the time is not the same as being
//! correct.
//!
//! So that case is its own variant —
//! [`Divergence::RefusedUnresolvedTargetThatRan`] — carrying
//! [`Attribution::Authoritative`], and it is subtracted from the plain
//! [`Divergence::Mutations`] difference rather than counted twice. Every other
//! finding carries the attribution the comparison can actually justify, which
//! is usually [`Attribution::Undetermined`]: a difference a human still has to
//! judge is reported as one.

use std::fmt;

use turnframe_core::case::CaseKey;
use turnframe_core::ids::OperationKey;
use turnframe_core::interaction::InteractionKind;
use turnframe_core::reduce::{CommandRef, PlannedActResult};
use turnframe_core::response::ClaimClass;
use turnframe_core::target::TargetResolution;

use crate::planning::PlannedTurn;

/// Which of the two paths a summary or a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    /// This library, running without authority.
    Shadow,
    /// The existing path, whose answer the user actually receives.
    Authoritative,
}

impl Side {
    /// The other one.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Shadow => Self::Authoritative,
            Self::Authoritative => Self::Shadow,
        }
    }

    /// A stable lower-case name, for a report or a metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Authoritative => "authoritative",
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One act a side extracted from the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActSummary {
    /// The shape of the act, as `ProposedAct::kind_name` spells it
    /// (`apply_operation`, `start_workflow`, …). An adopter describing a tool
    /// loop normally uses `apply_operation`.
    pub kind: String,
    /// The operation, when the act names one.
    pub operation: Option<OperationKey>,
    /// The case the act ended up aimed at, when the side resolved exactly one.
    pub case: Option<CaseKey>,
}

/// One mutation a side would run.
///
/// It is described by operation and case rather than by a domain payload,
/// because that is the level at which two different implementations of the same
/// intent are comparable at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationSummary {
    /// What it does.
    pub operation: OperationKey,
    /// What it changes, when the side resolved a case.
    pub case: Option<CaseKey>,
    /// The command inside the reduction, for the side that has one.
    pub command_ref: Option<CommandRef>,
}

/// One question a side asked before doing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClarificationSummary {
    /// The card's stable key within the turn.
    pub key: String,
    /// What kind of card it is.
    pub kind: InteractionKind,
    /// The case it belongs to.
    pub case: CaseKey,
}

/// Why a side did not run a mutation it had understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RefusalReason {
    /// Several authorized records matched, and picking one is never allowed
    /// (I8).
    AmbiguousTarget,
    /// The token was issued this turn and the record no longer exists.
    MissingTarget,
    /// Unknown token, or another tenant's.
    UnauthorizedTarget,
    /// The record moved past the revision the token was issued at.
    StaleTarget,
    /// Policy required a confirmation the turn did not have.
    ConfirmationRequired,
    /// The domain refused it deterministically.
    DomainRejected,
}

impl RefusalReason {
    /// Whether the refusal was about *which record was meant*.
    ///
    /// This is the class of refusal that makes a difference a finding against
    /// the other path: the mutation was understood, and the target was not.
    #[must_use]
    pub const fn is_unresolved_target(self) -> bool {
        matches!(
            self,
            Self::AmbiguousTarget
                | Self::MissingTarget
                | Self::UnauthorizedTarget
                | Self::StaleTarget
        )
    }

    /// A stable lower-case name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AmbiguousTarget => "ambiguous_target",
            Self::MissingTarget => "missing_target",
            Self::UnauthorizedTarget => "unauthorized_target",
            Self::StaleTarget => "stale_target",
            Self::ConfirmationRequired => "confirmation_required",
            Self::DomainRejected => "domain_rejected",
        }
    }
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A mutation a side understood and did not perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedMutation {
    /// What it would have done, when the act named an operation.
    pub operation: Option<OperationKey>,
    /// Why it did not.
    pub reason: RefusalReason,
}

/// One side's account of a turn.
///
/// Build the Turnframe side with
/// [`PlannedTurn::summary`](crate::planning::PlannedTurn::summary). Build the
/// other side by hand, from whatever the existing path logs — which is why this
/// struct is deliberately *not* `#[non_exhaustive]`: an adopter has to be able
/// to construct one. Prefer the `with_*` methods, or struct-update syntax over
/// [`TurnSummary::new`], so a later field arrives as a default rather than as a
/// compile error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnSummary {
    /// The cases this side addressed, in the order it considered them.
    pub cases: Vec<CaseKey>,
    /// The acts it extracted from the turn.
    pub acts: Vec<ActSummary>,
    /// The mutations it would run.
    pub mutations: Vec<MutationSummary>,
    /// The questions it asked before running anything.
    pub clarifications: Vec<ClarificationSummary>,
    /// The outcome classes it stated, or would have been entitled to state.
    pub claims: Vec<ClaimClass>,
    /// The outcome classes an event of this side actually backs. Empty for a
    /// planned turn, which by construction committed nothing.
    pub evidenced_claims: Vec<ClaimClass>,
    /// The mutations it understood and did not perform.
    pub refusals: Vec<RefusedMutation>,
}

impl TurnSummary {
    /// An empty summary, for a turn on which a side did nothing at all.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a case this side addressed.
    #[must_use]
    pub fn with_case(mut self, case: CaseKey) -> Self {
        self.cases.push(case);
        self
    }

    /// Adds an act this side extracted.
    #[must_use]
    pub fn with_act(mut self, act: ActSummary) -> Self {
        self.acts.push(act);
        self
    }

    /// Adds a mutation this side would run.
    #[must_use]
    pub fn with_mutation(mut self, mutation: MutationSummary) -> Self {
        self.mutations.push(mutation);
        self
    }

    /// Adds a question this side asked before running anything.
    #[must_use]
    pub fn with_clarification(mut self, clarification: ClarificationSummary) -> Self {
        self.clarifications.push(clarification);
        self
    }

    /// Adds an outcome class this side stated.
    #[must_use]
    pub fn with_claim(mut self, class: ClaimClass) -> Self {
        self.claims.push(class);
        self
    }

    /// Adds an outcome class an event of this side backs.
    #[must_use]
    pub fn with_evidenced_claim(mut self, class: ClaimClass) -> Self {
        self.evidenced_claims.push(class);
        self
    }

    /// Adds a mutation this side understood and did not perform.
    #[must_use]
    pub fn with_refusal(mut self, refusal: RefusedMutation) -> Self {
        self.refusals.push(refusal);
        self
    }

    /// This library's side of the comparison, read off a planned turn.
    ///
    /// `evidenced_claims` is empty and stays empty: planning commits nothing,
    /// so there is no event to cite. `claims` carries
    /// [`PlannedTurn::would_claim`](crate::planning::PlannedTurn::would_claim),
    /// which is an upper bound.
    #[must_use]
    pub fn from_planned(planned: &PlannedTurn) -> Self {
        let mut mutations: Vec<MutationSummary> = Vec::new();
        let mut acts: Vec<ActSummary> = Vec::new();
        let mut refusals: Vec<RefusedMutation> = Vec::new();

        for planned_act in &planned.reduction.acts {
            let case = planned_act
                .target
                .as_ref()
                .and_then(TargetResolution::exact)
                .map(turnframe_core::case::CaseRef::key);
            let operation = planned_act.act.operation().cloned();
            acts.push(ActSummary {
                kind: planned_act.act.kind_name().to_owned(),
                operation: operation.clone(),
                case: case.clone(),
            });
            match &planned_act.result {
                PlannedActResult::ReadyToExecute { command_refs } => {
                    if let Some(operation) = operation.clone() {
                        for command_ref in command_refs {
                            mutations.push(MutationSummary {
                                operation: operation.clone(),
                                case: case.clone(),
                                command_ref: Some(*command_ref),
                            });
                        }
                    }
                }
                PlannedActResult::AwaitingConfirmation { .. } => refusals.push(RefusedMutation {
                    operation: operation.clone(),
                    reason: RefusalReason::ConfirmationRequired,
                }),
                PlannedActResult::Rejected { .. } => refusals.push(RefusedMutation {
                    operation: operation.clone(),
                    reason: unresolved_reason(planned_act.target.as_ref())
                        .unwrap_or(RefusalReason::DomainRejected),
                }),
                PlannedActResult::NeedsClarification { .. } => {
                    if let Some(reason) = unresolved_reason(planned_act.target.as_ref()) {
                        refusals.push(RefusedMutation {
                            operation: operation.clone(),
                            reason,
                        });
                    }
                }
                _ => {}
            }
        }

        Self {
            cases: planned
                .views
                .iter()
                .map(|view| view.case_ref.key())
                .collect(),
            acts,
            mutations,
            clarifications: planned
                .would_persist
                .iter()
                .map(|spec| ClarificationSummary {
                    key: spec.key.clone(),
                    kind: spec.kind,
                    case: spec.case_ref.key(),
                })
                .collect(),
            claims: planned.would_claim.clone(),
            evidenced_claims: Vec::new(),
            refusals,
        }
    }
}

/// The refusal reason a target resolution implies, when it implies one.
fn unresolved_reason(resolution: Option<&TargetResolution>) -> Option<RefusalReason> {
    match resolution? {
        TargetResolution::Ambiguous { .. } => Some(RefusalReason::AmbiguousTarget),
        TargetResolution::Missing => Some(RefusalReason::MissingTarget),
        TargetResolution::Unauthorized => Some(RefusalReason::UnauthorizedTarget),
        TargetResolution::Stale { .. } => Some(RefusalReason::StaleTarget),
        TargetResolution::Exact { .. } => None,
        // `TargetResolution` is `#[non_exhaustive]`: a resolution this release
        // does not know is not evidence that the target was unresolved, so it
        // is not turned into a refusal reason.
        _ => None,
    }
}

/// Which path a finding is against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Attribution {
    /// This library behaved worse, or at least differently in a way it has to
    /// answer for.
    Shadow,
    /// The existing path did. A mutation on an ambiguous target is the case
    /// this module names.
    Authoritative,
    /// The comparison cannot say, and a person has to look.
    Undetermined,
}

impl Attribution {
    /// A stable lower-case name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Authoritative => "authoritative",
            Self::Undetermined => "undetermined",
        }
    }
}

impl fmt::Display for Attribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One way two paths disagreed about the same turn.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Divergence {
    /// The two sides addressed different records.
    CaseSelection {
        /// What this library selected.
        shadow: Vec<CaseKey>,
        /// What the existing path selected.
        authoritative: Vec<CaseKey>,
    },
    /// The two sides read different acts out of the same turn.
    ActsExtracted {
        /// What this library extracted.
        shadow: Vec<ActSummary>,
        /// What the existing path extracted.
        authoritative: Vec<ActSummary>,
    },
    /// The two sides would run different mutations, for a reason the
    /// comparison cannot attribute on its own.
    Mutations {
        /// What this library would run.
        shadow: Vec<MutationSummary>,
        /// What the existing path would run.
        authoritative: Vec<MutationSummary>,
    },
    /// One side asked the user something while the other acted.
    ClarificationVersusAction {
        /// The side that asked.
        asked: Side,
        /// What it asked.
        clarifications: Vec<ClarificationSummary>,
        /// What the other side did instead.
        mutations: Vec<MutationSummary>,
    },
    /// One side stated an outcome that no committed event, on either side,
    /// backs.
    ///
    /// Evidence from *either* side counts, because in a shadow stage only one
    /// side executes anything: a claim the authoritative path committed an
    /// event for is a backed claim, whoever else also made it. Only classes an
    /// event can back are checked — "you can see the card below" is backed by a
    /// persisted card and "I will notify you" by nothing at all, so neither is
    /// judged here.
    ClaimWithoutEvent {
        /// The side that stated it.
        claimant: Side,
        /// What it stated.
        class: ClaimClass,
    },
    /// This library refused a mutation because it could not tell which record
    /// was meant, and the existing path performed it anyway.
    ///
    /// This is the asymmetry this module exists to carry, and it is a finding
    /// against the existing path: it chose one of several records the user
    /// might have meant.
    RefusedUnresolvedTargetThatRan {
        /// The mutation the existing path performed.
        performed: MutationSummary,
        /// Why this library would not.
        reason: RefusalReason,
    },
}

impl Divergence {
    /// A stable lower-case name of the kind, for a metric label or a column.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::CaseSelection { .. } => "case_selection",
            Self::ActsExtracted { .. } => "acts_extracted",
            Self::Mutations { .. } => "mutations",
            Self::ClarificationVersusAction { .. } => "clarification_versus_action",
            Self::ClaimWithoutEvent { .. } => "claim_without_event",
            Self::RefusedUnresolvedTargetThatRan { .. } => "refused_unresolved_target_that_ran",
        }
    }
}

/// A divergence together with the side it counts against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// What differed.
    pub divergence: Divergence,
    /// Whose problem it is, as far as the comparison can tell.
    pub attribution: Attribution,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.divergence.kind(), self.attribution)
    }
}

/// Everything two summaries of one turn disagreed about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DivergenceReport {
    /// The findings, in the order [`compare`] produces them.
    pub findings: Vec<Finding>,
}

impl DivergenceReport {
    /// Whether the two sides agreed on everything the vocabulary covers.
    #[must_use]
    pub fn agreed(&self) -> bool {
        self.findings.is_empty()
    }

    /// The findings against one side.
    pub fn against(&self, side: Side) -> impl Iterator<Item = &Finding> {
        let wanted = match side {
            Side::Shadow => Attribution::Shadow,
            Side::Authoritative => Attribution::Authoritative,
        };
        self.findings
            .iter()
            .filter(move |finding| finding.attribution == wanted)
    }

    /// Whether anything at all counts against this library.
    ///
    /// This is the question a migration actually asks, and it is not
    /// "were there differences": a run whose only findings are against the
    /// existing path is a run that went well.
    #[must_use]
    pub fn any_against_shadow(&self) -> bool {
        self.against(Side::Shadow).next().is_some()
    }

    /// The findings of one kind, by the name [`Divergence::kind`] gives.
    pub fn of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Finding> {
        self.findings
            .iter()
            .filter(move |finding| finding.divergence.kind() == kind)
    }
}

impl fmt::Display for DivergenceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.findings.is_empty() {
            return f.write_str("no divergence");
        }
        for (index, finding) in self.findings.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{finding}")?;
        }
        Ok(())
    }
}

/// Compares the two sides of one turn.
///
/// The order of the findings is stable: the attributable asymmetry first, then
/// case selection, acts, mutations, clarification-versus-action, and claims.
#[must_use]
pub fn compare(shadow: &TurnSummary, authoritative: &TurnSummary) -> DivergenceReport {
    let mut findings = Vec::new();

    // A mutation the existing path ran that this library refused because it
    // could not tell which record was meant. Reported first, and subtracted
    // from the plain mutation difference below so it is not counted twice.
    let mut explained: Vec<&MutationSummary> = Vec::new();
    for performed in &authoritative.mutations {
        if shadow
            .mutations
            .iter()
            .any(|ours| same_mutation(ours, performed))
        {
            continue;
        }
        let Some(refusal) = shadow.refusals.iter().find(|refusal| {
            refusal.reason.is_unresolved_target()
                && refusal
                    .operation
                    .as_ref()
                    .is_none_or(|operation| *operation == performed.operation)
        }) else {
            continue;
        };
        explained.push(performed);
        findings.push(Finding {
            divergence: Divergence::RefusedUnresolvedTargetThatRan {
                performed: performed.clone(),
                reason: refusal.reason,
            },
            attribution: Attribution::Authoritative,
        });
    }

    if shadow.cases != authoritative.cases {
        findings.push(Finding {
            divergence: Divergence::CaseSelection {
                shadow: shadow.cases.clone(),
                authoritative: authoritative.cases.clone(),
            },
            attribution: Attribution::Undetermined,
        });
    }

    if shadow.acts != authoritative.acts {
        findings.push(Finding {
            divergence: Divergence::ActsExtracted {
                shadow: shadow.acts.clone(),
                authoritative: authoritative.acts.clone(),
            },
            attribution: Attribution::Undetermined,
        });
    }

    let remaining: Vec<MutationSummary> = authoritative
        .mutations
        .iter()
        .filter(|performed| !explained.iter().any(|done| same_mutation(done, performed)))
        .cloned()
        .collect();
    if !same_mutation_set(&shadow.mutations, &remaining) {
        findings.push(Finding {
            divergence: Divergence::Mutations {
                shadow: shadow.mutations.clone(),
                authoritative: remaining,
            },
            attribution: Attribution::Undetermined,
        });
    }

    for asked in [Side::Shadow, Side::Authoritative] {
        let (asker, actor) = match asked {
            Side::Shadow => (shadow, authoritative),
            Side::Authoritative => (authoritative, shadow),
        };
        if asker.clarifications.is_empty()
            || !asker.mutations.is_empty()
            || actor.mutations.is_empty()
        {
            continue;
        }
        // Asking which record was meant, while the other side picked one, is
        // that asymmetry again. The other direction — the
        // existing path asked and this library acted — depends on what the user
        // meant, which the comparison does not know.
        let attribution = if asked == Side::Shadow
            && asker
                .refusals
                .iter()
                .any(|refusal| refusal.reason.is_unresolved_target())
        {
            Attribution::Authoritative
        } else {
            Attribution::Undetermined
        };
        findings.push(Finding {
            divergence: Divergence::ClarificationVersusAction {
                asked,
                clarifications: asker.clarifications.clone(),
                mutations: actor.mutations.clone(),
            },
            attribution,
        });
    }

    for (side, summary) in [(Side::Shadow, shadow), (Side::Authoritative, authoritative)] {
        for class in &summary.claims {
            if !is_event_backable(*class)
                || shadow.evidenced_claims.contains(class)
                || authoritative.evidenced_claims.contains(class)
            {
                continue;
            }
            findings.push(Finding {
                divergence: Divergence::ClaimWithoutEvent {
                    claimant: side,
                    class: *class,
                },
                attribution: match side {
                    Side::Shadow => Attribution::Shadow,
                    Side::Authoritative => Attribution::Authoritative,
                },
            });
        }
    }

    DivergenceReport { findings }
}

/// Whether a committed event is the kind of thing that could back this class.
///
/// [`ClaimClass::InteractionVisibility`] is backed by a persisted card and
/// [`ClaimClass::FutureNotification`] by nothing the library has, so neither
/// can be judged against the ledger.
const fn is_event_backable(class: ClaimClass) -> bool {
    !matches!(
        class,
        ClaimClass::InteractionVisibility | ClaimClass::FutureNotification
    )
}

/// Two mutations are the same when they do the same thing to the same record.
/// The command reference is deliberately ignored: only one side has one.
fn same_mutation(left: &MutationSummary, right: &MutationSummary) -> bool {
    left.operation == right.operation && left.case == right.case
}

fn same_mutation_set(left: &[MutationSummary], right: &[MutationSummary]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(one, other)| same_mutation(one, other))
}
