//! What an adopter can actually name through the facade.
//!
//! The README tells an application to depend on `turnframe` and nothing else,
//! and the feature flags make that the obvious choice. So the facade is the
//! surface, and a type missing from it is missing from the library as far as
//! most adopters are concerned.
//!
//! The failure this file exists to catch is quiet, which is why it needs a
//! test rather than a review. A module left out of the re-export list breaks
//! nothing that anybody runs: the workspace compiles, every crate's own tests
//! pass, and the examples keep working because they reach for the parts that
//! were remembered. It even half-works for the adopter, which is the worst
//! part. `let planner = orchestrator.planner();` binds the value and every
//! method on it does its job, so the gap does not show while you are
//! exploring. It shows when you try to write it down — a builder that returns
//! a configured planner, a struct field holding one, a function taking a
//! `PlannedTurn` — because a signature has to name a type and there is no path
//! to name.
//!
//! Two kinds of check, because they fail at different moments.
//!
//! [`every_runtime_module_is_reachable_through_the_facade`] reads the runtime
//! crate's own module list and compares it with the facade's. It catches the
//! *next* omission: a module added to `turnframe-runtime` and not to the
//! re-export beside it, which is how the last one happened.
//!
//! The modules below name real types through the facade and put them in
//! positions a bare `use` would not prove — a struct field, a function
//! parameter, a return type, a trait `impl`. Those are compile-time checks.
//! They do not run so much as refuse to build, which is the point: they fail
//! the way an adopter's code fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/// A crate's public modules, read from its source rather than from a list
/// somebody has to remember to update.
fn declared_modules(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            // `mod readme {}`, `pub mod prelude { … }` and other inline modules
            // carry a body rather than a semicolon, so they never match;
            // anything that does is a public module with a file behind it.
            let name = line.trim().strip_prefix("pub mod ")?.strip_suffix(';')?;
            Some(name.trim().to_owned())
        })
        .collect()
}

/// The runtime's public modules.
fn runtime_modules() -> Vec<String> {
    declared_modules(include_str!("../../turnframe-runtime/src/lib.rs"))
}

/// The core crate's public modules.
fn core_modules() -> Vec<String> {
    declared_modules(include_str!("../../turnframe-core/src/lib.rs"))
}

/// The names the facade re-exports under `turnframe::runtime`.
fn facade_modules() -> Vec<String> {
    const OPENING: &str = "pub use turnframe_runtime::{";
    let source = include_str!("../src/lib.rs");
    let start = source
        .find(OPENING)
        .expect("the facade re-exports a block of runtime modules");
    let rest = &source[start + OPENING.len()..];
    let end = rest.find("};").expect("the re-export block is closed");
    rest[..end]
        .split(',')
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

#[test]
fn every_runtime_module_is_reachable_through_the_facade() {
    let declared = runtime_modules();
    assert!(
        declared.len() > 10,
        "the runtime's module list did not parse, so this test proves nothing: {declared:?}"
    );
    let exported = facade_modules();

    let missing: Vec<&String> = declared
        .iter()
        .filter(|name| !exported.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "these runtime modules are not re-exported by the facade, so an adopter depending on \
         `turnframe` cannot name any type in them: {missing:?}. Add them to the \
         `pub use turnframe_runtime::{{…}}` block in crates/turnframe/src/lib.rs."
    );

    // The other direction is worth checking too. A name the runtime no longer
    // declares would fail to compile, but the assertion says which one rather
    // than leaving a rename to be read out of a resolver error.
    let unknown: Vec<&String> = exported
        .iter()
        .filter(|name| !declared.contains(name))
        .collect();
    assert!(
        unknown.is_empty(),
        "the facade re-exports names the runtime does not declare: {unknown:?}"
    );
}

#[test]
fn every_core_module_is_reachable_through_the_facade() {
    // The facade wraps each core module by hand so it can carry a sentence of
    // its own, so what marks one as covered is the glob inside that wrapper.
    // A curated root re-export names its items in braces and is deliberately
    // not enough: `pub use turnframe_core::plan::{UserTurnPlan, …}` puts five
    // types within reach and leaves the rest of the module with no path at all,
    // which is exactly how `AnswerBasis` went missing while `UserTurnPlan` was
    // fine.
    let facade = include_str!("../src/lib.rs");
    let missing: Vec<String> = core_modules()
        .into_iter()
        .filter(|name| !facade.contains(&format!("pub use turnframe_core::{name}::*;")))
        .collect();
    assert!(
        missing.is_empty(),
        "these core modules have no module of their own in the facade, so anything in them that \
         is not individually re-exported at the root cannot be named at all: {missing:?}. Add a \
         `pub mod` wrapper for each in crates/turnframe/src/lib.rs."
    );
}

#[test]
fn a_public_type_always_lives_in_a_public_module() {
    // This is what makes the two checks above sufficient, and it is worth
    // asserting rather than assuming.
    //
    // The facade wraps `turnframe-core` and `turnframe-runtime` module by
    // module, each with a glob, and a glob carries a module's public submodules
    // along with its types. So *if* everything public in those crates is
    // reachable from one of their top-level public modules, then covering every
    // module covers every type, and no separate type-level check is needed.
    //
    // What would break the implication is a type declared in a private module
    // and re-exported from somewhere else. It would be public, would belong to
    // no module the facade wraps, and would be unreachable through the facade
    // while every other check here stayed green — which is how `Digest` and
    // `AnswerBasis` came to be found by an adopter rather than by this suite.
    //
    // The rule is narrower than it looks. It applies only to the two crates the
    // facade wraps by hand; the store and provider crates are globbed whole, so
    // anything they re-export arrives on its own.
    for (crate_name, source) in [
        (
            "turnframe-core",
            include_str!("../../turnframe-core/src/lib.rs"),
        ),
        (
            "turnframe-runtime",
            include_str!("../../turnframe-runtime/src/lib.rs"),
        ),
    ] {
        let modules = declared_modules(source);
        let smuggled: Vec<&str> = source
            .lines()
            .map(str::trim)
            .filter_map(|line| line.strip_prefix("pub use crate::"))
            .filter(|rest| {
                let first = rest.split(&[':', '{', ';'][..]).next().unwrap_or_default();
                !first.is_empty() && !modules.iter().any(|module| module == first)
            })
            .collect();
        assert!(
            smuggled.is_empty(),
            "{crate_name} makes types public from somewhere that is not one of its public \
             modules: {smuggled:?}. The facade wraps this crate one module at a time, so a type \
             that reaches the outside any other way reaches no facade module at all. Give it a \
             public module, or add it to the facade's root and prelude deliberately."
        );
    }
}

/// The shadow path, written the way an integration writes it.
///
/// The migration guide makes shadowing a mandatory stage before cutover, so
/// this is the one surface an adopter is guaranteed to need. Each name here
/// appears in a *position* rather than only in a `use`, which is the
/// distinction the omission hid behind: calling a method on a value you cannot
/// name compiles perfectly well.
mod the_shadow_path_can_be_written_down {
    use turnframe::runtime::divergence::TurnSummary;
    use turnframe::runtime::planning::{PlannedTurn, SeededCase, SeededTurnPlanner, TurnPlanner};

    /// A field of the adopter's own type, which is what a shadow runner holds.
    #[allow(dead_code)]
    struct ShadowRunner {
        planner: TurnPlanner,
    }

    /// The seeded variant, which holds no persistence at all, beside the cases
    /// it is seeded with.
    #[allow(dead_code)]
    struct CorpusReplayer {
        planner: SeededTurnPlanner,
        cases: Vec<SeededCase>,
    }

    /// A planned turn in a parameter and a summary in a return type, which is
    /// the shape of every comparison an adopter writes.
    #[allow(dead_code)]
    fn summarize(planned: &PlannedTurn) -> TurnSummary {
        planned.summary()
    }

    /// The three fields whose verb is deliberate, which the migration guide
    /// names one by one because they are what a shadow run is read for.
    #[allow(dead_code)]
    fn would_have_done(planned: &PlannedTurn) -> (usize, usize, usize) {
        (
            planned.would_persist.len(),
            planned.would_execute.len(),
            planned.would_claim.len(),
        )
    }
}

/// The divergence vocabulary, which the shadow stage names directly.
///
/// A report is meant to become a column in a table and a label on a metric, so
/// an adopter writes types over all of it rather than reading it once and
/// throwing it away.
mod the_divergence_vocabulary_can_be_written_down {
    use turnframe::runtime::divergence::{
        Attribution, Divergence, DivergenceReport, Finding, RefusalReason, Side, TurnSummary,
        compare,
    };

    /// A row in the adopter's table.
    #[allow(dead_code)]
    struct ComparisonRow {
        report: DivergenceReport,
        blamed: Attribution,
        looked_at: Side,
    }

    /// The comparison itself, in a signature.
    #[allow(dead_code)]
    fn run(shadow: &TurnSummary, authoritative: &TurnSummary) -> DivergenceReport {
        compare(shadow, authoritative)
    }

    /// The pair of labels a metric carries.
    #[allow(dead_code)]
    fn label(finding: &Finding) -> (&'static str, &'static str) {
        let kind: &'static str = Divergence::kind(&finding.divergence);
        (kind, finding.attribution.as_str())
    }

    /// A refusal reason in a `match`, which is what turns one into copy, and
    /// the predicate the migration guide leans on to separate a target the
    /// shadow could not resolve from a refusal for any other reason.
    #[allow(dead_code)]
    fn blames_the_target(reason: &RefusalReason) -> bool {
        matches!(reason, RefusalReason::AmbiguousTarget) || reason.is_unresolved_target()
    }

    /// The asymmetric finding, matched by name: a mutation this library refused
    /// for an unresolved target that the existing path performed anyway.
    #[allow(dead_code)]
    fn is_the_asymmetry(divergence: &Divergence) -> bool {
        matches!(
            divergence,
            Divergence::RefusedUnresolvedTargetThatRan { .. }
        )
    }
}

/// The outbox, which an adopter does not merely name but *implements*.
///
/// Delivering an external effect means writing `impl OutboxSender for …`, and
/// a trait that cannot be named cannot be implemented at all. Of the modules
/// that were missing, this is the one where the omission was a wall rather
/// than an inconvenience.
///
/// The `async_trait` attribute comes from the adopter's own dependency, as it
/// does in `examples/console`. The facade does not re-export it, which is the
/// ordinary Rust arrangement.
mod the_outbox_traits_can_be_implemented {
    use async_trait::async_trait;
    use turnframe::event::OutboxEntry;
    use turnframe::runtime::dispatch::{
        DispatchConfig, DispatchReport, Dispatched, OutboxDispatcher, OutboxSender,
    };

    struct AlwaysAccepts;

    #[async_trait]
    impl OutboxSender for AlwaysAccepts {
        async fn send(&self, _entry: &OutboxEntry) -> Dispatched {
            Dispatched::Completed { remote_ref: None }
        }
    }

    /// The dispatcher and its configuration, in the adopter's own signatures.
    #[allow(dead_code)]
    fn configure(config: DispatchConfig) -> DispatchConfig {
        config
    }

    #[allow(dead_code)]
    fn unresolved(report: &DispatchReport) -> usize {
        report.unknown.len() + report.unsettled.len()
    }

    #[allow(dead_code)]
    fn hand_over(dispatcher: OutboxDispatcher) -> OutboxDispatcher {
        dispatcher
    }

    #[allow(dead_code)]
    fn as_sender(sender: &AlwaysAccepts) -> &dyn OutboxSender {
        sender
    }
}

/// Budgets and resumption: configuration and control flow an application owns,
/// rather than internals it only observes.
mod budgets_and_resumption_can_be_written_down {
    use turnframe::runtime::budget::{BudgetLimit, BudgetSpend, TurnBudget};
    use turnframe::runtime::resume::{DeferredAct, Resumption};

    #[allow(dead_code)]
    struct Limits {
        budget: TurnBudget,
    }

    #[allow(dead_code)]
    fn over(
        budget: &TurnBudget,
        spent: BudgetSpend,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<BudgetLimit> {
        budget.exhausted(spent, now)
    }

    #[allow(dead_code)]
    fn continues(resumption: &Resumption) -> bool {
        resumption.act().is_some()
    }

    #[allow(dead_code)]
    fn deferred_key(act: &DeferredAct) -> &DeferredAct {
        act
    }
}

/// Everything a prelude-level type needs in order to be *built* from the
/// prelude.
///
/// The rule this guards is narrower than "the prelude is complete", which would
/// be untrue and unhelpful: plenty of what an application receives — an
/// `AssistantTurn`, an `OperationalReceipt`, an `InteractionView` — is read
/// rather than constructed, and the types inside it can live wherever they
/// read best. The rule is about the ones an adopter constructs. If the prelude
/// offers a type, the prelude has to offer what its constructor asks for,
/// because a `use turnframe::prelude::*;` that leaves you hunting for a second
/// import is a prelude that did not do its job.
///
/// `CommandOrigin` was the case that showed this was not holding. It sits in
/// the prelude and at the root, and three of its field types did not: `Digest`
/// lived only under `turnframe::schema`, which is not where anyone looks for a
/// hash, while `ActionClass` and `ResolutionChannel` were at the root but not
/// in the prelude beside the enum that needs them. Every arm below is written
/// as an adopter writes it, from the prelude and nothing else.
mod prelude_level_types_can_be_built_from_the_prelude {
    use turnframe::prelude::*;

    /// Every arm of the origin enum, which is the one an adopter reaches for
    /// when testing their own executor against a command it did not issue.
    #[allow(dead_code)]
    fn every_command_origin() -> Vec<CommandOrigin> {
        vec![
            CommandOrigin::DirectSafeUserAct {
                evidence_digest: Digest::of_bytes(b"evidence"),
            },
            CommandOrigin::ConfirmedInteraction {
                interaction_id: InteractionId::new(),
                payload_hash: Digest::of_bytes(b"payload"),
                interaction_kind: InteractionKind::ConfirmCommand,
                action_class: ActionClass::ConfirmsCommands,
                channel: ResolutionChannel::Click,
            },
            CommandOrigin::InternalPolicy {
                policy_key: String::from("nightly-reconcile"),
            },
            CommandOrigin::ExternalCallback {
                callback_id: String::from("cb-1"),
                signature_verified: true,
            },
        ]
    }

    /// A rejection an executor returns, whose code is its own vocabulary.
    #[allow(dead_code)]
    fn a_rejection() -> DomainRejection {
        DomainRejection::new(RejectionCode::from("trip.locked"), "trip.locked.message")
    }

    /// The localized copy that goes with it, in both languages the workflow
    /// answers in.
    #[allow(dead_code)]
    fn its_copy() -> LocalizedText {
        LocalizedText::new("That trip is locked.")
            .with(Locale::from("it-IT"), "Quel viaggio è bloccato.")
    }

    /// The actor a turn runs as, which needs both identifiers.
    #[allow(dead_code)]
    fn an_actor() -> ActorContext {
        ActorContext::new(AccountId::from("acct-1"), UserId::from("user-1"))
    }
}

/// A response block, built from outside the library.
///
/// This is the check the reported gap needed. `ResponseBlock` was at the root
/// and in the prelude, and `AnswerBasis` — which one of its variants carries —
/// had no path through the facade at all: not at the root, not in the prelude,
/// and not in a module, because the facade had no `plan` module for it to live
/// in. Five types from `turnframe_core::plan` were re-exported individually at
/// the root, which made the module look covered while the rest of it was
/// unreachable.
///
/// An adopter builds a response block when they compose their own answer, and
/// when they write a test that asserts on one, so this is written the way both
/// of those are written.
mod a_response_block_can_be_built_from_outside {
    use turnframe::plan::AnswerBasis;
    use turnframe::prelude::*;
    use turnframe::response::{AnswerStatus, GeneratedAnswer};

    #[allow(dead_code)]
    fn an_answer(block_id: turnframe::ids::BlockId) -> ResponseBlock {
        ResponseBlock::Answer(GeneratedAnswer {
            block_id,
            question_id: None,
            text: String::from("The trip is called Lisbon."),
            basis: AnswerBasis::CurrentCommittedState,
            status: AnswerStatus::Answered,
            facts_used: Vec::new(),
            citations: Vec::new(),
            enumerations: Vec::new(),
        })
    }

    /// Every basis an answer may rest on, since which one a block declares is
    /// the difference between describing committed state and describing a
    /// proposal.
    #[allow(dead_code)]
    fn every_basis() -> Vec<AnswerBasis> {
        vec![
            AnswerBasis::CurrentCommittedState,
            AnswerBasis::ProposedState,
            AnswerBasis::CommittedStateAfterTurn,
            AnswerBasis::GeneralDomainKnowledge,
        ]
    }
}
