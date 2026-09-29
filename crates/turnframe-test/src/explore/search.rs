//! The breadth-first search itself.

use std::collections::{BTreeSet, VecDeque};

use turnframe_core::case::CaseRef;
use turnframe_core::flow::{PhaseOwnership, ViewOf, WorkflowDefinition, check_view};
use turnframe_core::hash::Digest;
use turnframe_core::hash::canonical_value;
use turnframe_core::ids::{CaseId, CaseRevision};
use turnframe_core::target::{ResolvedAct, ResolvedActKind};

use crate::explore::{
    ExplorationLimits, ExplorationReport, ExplorationViolation, ExplorationViolationKind,
    SimulatedTransition, WorkflowModel,
};

/// Case identifier every explored projection is made against. Exploration never
/// touches a store, so the identifier only has to be stable.
pub const EXPLORATION_CASE_ID: &str = "explore";

/// The case revisions exploration projects at, cycled by visit order.
///
/// Deliberately **not** derived from the depth. A revision that counted the
/// commands would be perfectly correlated with the path, so a projector that
/// reads the revision would produce a different view for every state and never
/// look wrong; cycling a short, uneven table instead means the same state is
/// projected at two unrelated revisions and two states at the same depth are
/// projected at different ones. `0` is in the table because it is the revision
/// of a case that does not exist yet, which is the value a projector is most
/// likely to special-case by accident.
pub const EXPLORATION_REVISIONS: [u64; 4] = [0, 1, 7, 4_096];

/// Explores the states reachable from `model`'s initial states and checks the
/// projection invariants of spec §8.4 on every one of them.
///
/// The search is breadth-first, so the path reported with a violation is the
/// shortest sequence of commands that reaches the offending state. States are
/// deduplicated by the canonical JSON of the state, which is why a model must
/// derive identifiers deterministically.
///
/// Checked on every reachable state:
///
/// * [`check_view`] passes — one phase, obligation identifiers unique, a
///   user-owned phase carries a blocking requirement, a terminal phase carries
///   none and does carry an outcome, no outcome while obligations remain;
/// * two projections of the same state produce the same obligation identifiers
///   and the same erased view (I2);
/// * projecting the same state at another case revision produces the same view
///   once the case reference itself is set aside, so the map cannot depend on
///   how often the case was written (see [`EXPLORATION_REVISIONS`]);
/// * the blocking requirement, when there is one, builds into a card that can
///   actually be answered ([`InteractionSpec::validate`]), so a user-owned
///   phase really does derive an interaction (spec §27.3);
/// * the state is not a dead end: it is terminal, or it offers a candidate
///   command, or it carries a blocking interaction;
/// * an *absent* state does not project to a terminal phase or an outcome. A
///   case's identity outlives its content, so removal is a status and an absent
///   state means *not yet*, never *no longer*
///   ([`ExplorationViolationKind::CaseEndsByDisappearing`]).
///
/// [`InteractionSpec::validate`]: turnframe_core::interaction::InteractionSpec::validate
///
/// Checked on every simulated transition:
///
/// * a refused command leaves the state it was given untouched;
/// * a command [`WorkflowDefinition::validate_command`] refuses is not applied
///   by the model;
/// * an applied command does not drop the case it was given
///   ([`ExplorationViolationKind::TransitionRemovesCase`]), which is the same
///   rule seen from the model instead of from the projector.
///
/// Checked once at the end, and only when the search was **not** truncated:
/// every outcome [`WorkflowModel::declared_outcomes`] declares was projected
/// somewhere. A truncated search cannot prove that an outcome is out of reach,
/// only that it did not get there within the limits, so the check is skipped
/// rather than reported as a violation.
#[must_use]
pub fn explore<W, M>(definition: &W, model: &M, limits: ExplorationLimits) -> ExplorationReport
where
    W: WorkflowDefinition,
    M: WorkflowModel<W> + ?Sized,
{
    let mut explorer = Explorer::new(definition, limits);
    let mut queue: VecDeque<Node<W::State>> = VecDeque::new();
    for state in model.initial_states() {
        if let Some(node) = explorer.admit(state, 0, Vec::new()) {
            queue.push_back(node);
        }
    }
    while let Some(node) = queue.pop_front() {
        explorer.states_explored += 1;
        explorer.max_depth_reached = explorer.max_depth_reached.max(node.depth);
        let facts = explorer.inspect(&node);
        queue.extend(explorer.expand(model, &node, facts));
    }
    explorer.finish(model)
}

/// One state in the frontier, with the shortest path that reached it.
struct Node<S> {
    state: Option<S>,
    state_json: serde_json::Value,
    depth: usize,
    path: Vec<serde_json::Value>,
}

/// What the projection of a state says about expanding it.
#[derive(Clone, Copy)]
struct StateFacts {
    terminal: bool,
    has_blocking_interaction: bool,
}

struct Explorer<'a, W: WorkflowDefinition> {
    definition: &'a W,
    limits: ExplorationLimits,
    seen: BTreeSet<String>,
    reached_phases: Vec<serde_json::Value>,
    reached_outcomes: Vec<serde_json::Value>,
    violations: Vec<ExplorationViolation>,
    states_explored: usize,
    transitions_simulated: usize,
    max_depth_reached: usize,
    truncated: bool,
}

impl<'a, W: WorkflowDefinition> Explorer<'a, W> {
    fn new(definition: &'a W, limits: ExplorationLimits) -> Self {
        Self {
            definition,
            limits,
            seen: BTreeSet::new(),
            reached_phases: Vec::new(),
            reached_outcomes: Vec::new(),
            violations: Vec::new(),
            states_explored: 0,
            transitions_simulated: 0,
            max_depth_reached: 0,
            truncated: false,
        }
    }

    /// Records a state as visited and turns it into a frontier node, unless it
    /// was seen before or the state budget is spent.
    fn admit(
        &mut self,
        state: Option<W::State>,
        depth: usize,
        path: Vec<serde_json::Value>,
    ) -> Option<Node<W::State>> {
        let Ok(state_json) = canonical_value(&state) else {
            self.violations.push(ExplorationViolation {
                kind: ExplorationViolationKind::UnserializableState,
                state: serde_json::Value::Null,
                depth,
                path,
            });
            return None;
        };
        let key = state_json.to_string();
        if self.seen.contains(&key) {
            return None;
        }
        if self.seen.len() >= self.limits.max_states {
            self.truncated = true;
            return None;
        }
        self.seen.insert(key);
        Some(Node {
            state,
            state_json,
            depth,
            path,
        })
    }

    fn violate(&mut self, kind: ExplorationViolationKind, node: &Node<W::State>) {
        self.violations.push(ExplorationViolation {
            kind,
            state: node.state_json.clone(),
            depth: node.depth,
            path: node.path.clone(),
        });
    }

    /// The case reference an explored projection is made against.
    fn case_ref_at(&self, revision: u64) -> CaseRef {
        CaseRef::new(
            self.definition.key(),
            CaseId::from(EXPLORATION_CASE_ID),
            CaseRevision(revision),
        )
    }

    /// Projects the state three times — twice at one revision, once at another
    /// — and checks every per-state rule.
    fn inspect(&mut self, node: &Node<W::State>) -> StateFacts {
        let slot = self.states_explored % EXPLORATION_REVISIONS.len();
        let revision = EXPLORATION_REVISIONS[slot];
        let other_revision = EXPLORATION_REVISIONS[(slot + 1) % EXPLORATION_REVISIONS.len()];
        let case_ref = self.case_ref_at(revision);
        let first = self
            .definition
            .project(case_ref.clone(), node.state.as_ref());
        let second = self.definition.project(case_ref, node.state.as_ref());
        self.check_revision_independence(node, &first, revision, other_revision);
        if let Err(violations) = check_view(self.definition, &first) {
            for violation in violations {
                self.violate(ExplorationViolationKind::Projection(violation), node);
            }
        }
        let ownership = self.definition.phase_ownership(&first.phase);
        let facts = StateFacts {
            terminal: ownership == PhaseOwnership::Terminal || first.outcome.is_some(),
            has_blocking_interaction: first.blocking_interaction.is_some(),
        };
        self.check_absent_state_is_not_terminal(node, &first, facts);
        self.check_blocking_interaction(node, &first);
        self.check_catalogued_operations_compile(node, &first);
        let erased = (
            first.erase(ownership),
            second.erase(self.definition.phase_ownership(&second.phase)),
        );
        let (Ok(first), Ok(second)) = erased else {
            self.violate(ExplorationViolationKind::UnserializableState, node);
            return facts;
        };
        let ids = |view: &turnframe_core::flow::ErasedWorkflowView| {
            view.obligations
                .iter()
                .map(|o| o.id.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        let (first_ids, second_ids) = (ids(&first), ids(&second));
        if first_ids != second_ids {
            self.violate(
                ExplorationViolationKind::UnstableObligationIds {
                    first: first_ids,
                    second: second_ids,
                },
                node,
            );
        } else if first != second {
            self.violate(
                ExplorationViolationKind::NonDeterministicProjection {
                    first: canonical_value(&first).unwrap_or(serde_json::Value::Null),
                    second: canonical_value(&second).unwrap_or(serde_json::Value::Null),
                },
                node,
            );
        }
        if !self.reached_phases.contains(&first.phase) {
            self.reached_phases.push(first.phase.clone());
        }
        if let Some(outcome) = first.outcome
            && !self.reached_outcomes.contains(&outcome)
        {
            self.reached_outcomes.push(outcome);
        }
        facts
    }

    /// Asks this state's catalogue whether `compile_act` knows its operations.
    ///
    /// The same walk the rest of the explorer already does, and it belongs here
    /// for the same reason the phase-ownership check does: a catalogue that
    /// offers what the compiler does not know is a property of a workflow, not
    /// of a deployment.
    ///
    /// Only [`turnframe_core::error::UNKNOWN_OPERATION`] counts. The arguments
    /// are `null`, which most operations will refuse for an honest reason, so
    /// any other rejection is left alone — a check that demanded good arguments
    /// would be testing whether the explorer can invent them.
    fn check_catalogued_operations_compile(&mut self, node: &Node<W::State>, view: &ViewOf<W>) {
        for definition in self.definition.operations(view) {
            let act = ResolvedAct {
                act: turnframe_core::understanding::ActId::new(
                    turnframe_core::understanding::UnitId(1),
                    1,
                ),
                kind: ResolvedActKind::ApplyOperation {
                    operation: definition.key.clone(),
                },
                case_ref: view.case_ref.clone(),
                arguments: serde_json::Value::Null,
                evidence_digest: Digest::of_bytes(b""),
            };
            if let Err(rejection) = self.definition.compile_act(node.state.as_ref(), view, &act)
                && rejection.code.as_str() == turnframe_core::error::UNKNOWN_OPERATION
            {
                self.violate(
                    ExplorationViolationKind::CatalogedOperationDoesNotCompile {
                        operation: definition.key.clone(),
                    },
                    node,
                );
            }
        }
    }

    /// Checks that the view does not change when the same state is projected at
    /// another case revision.
    ///
    /// The case reference itself legitimately differs, so it is normalized away
    /// before the comparison: what must hold is that everything *derived* from
    /// the state is the same.
    fn check_revision_independence(
        &mut self,
        node: &Node<W::State>,
        first: &ViewOf<W>,
        revision: u64,
        other_revision: u64,
    ) {
        let other = self
            .definition
            .project(self.case_ref_at(other_revision), node.state.as_ref());
        let erased = (
            first.erase(self.definition.phase_ownership(&first.phase)),
            other.erase(self.definition.phase_ownership(&other.phase)),
        );
        let (Ok(first), Ok(mut other)) = erased else {
            self.violate(ExplorationViolationKind::UnserializableState, node);
            return;
        };
        other.case_ref = first.case_ref.clone();
        if let Some(detail) = view_difference(&first, &other) {
            self.violate(
                ExplorationViolationKind::ProjectionVariesWithRevision {
                    left: revision,
                    right: other_revision,
                    detail: detail.to_owned(),
                },
                node,
            );
        }
    }

    /// Checks that an absent state does not project to a terminal phase or an
    /// outcome.
    ///
    /// The executor answers `None` both for a case nobody has created and for a
    /// case whose row was deleted, so a projector that reads absence as
    /// completion has one view doing two opposite jobs. The library's position
    /// is that the second reading is wrong: a case's identity outlives its
    /// content, removal is a status, and an absent state means *not yet*, never
    /// *no longer*.
    fn check_absent_state_is_not_terminal(
        &mut self,
        node: &Node<W::State>,
        view: &ViewOf<W>,
        facts: StateFacts,
    ) {
        if node.state.is_some() || !facts.terminal {
            return;
        }
        let phase = canonical_value(&view.phase).unwrap_or(serde_json::Value::Null);
        let outcome = view
            .outcome
            .as_ref()
            .map(|outcome| canonical_value(outcome).unwrap_or(serde_json::Value::Null));
        self.violate(
            ExplorationViolationKind::CaseEndsByDisappearing { phase, outcome },
            node,
        );
    }

    /// Checks that the blocking requirement of a phase builds into a card the
    /// user can answer (I6, spec §27.3).
    fn check_blocking_interaction(&mut self, node: &Node<W::State>, view: &ViewOf<W>) {
        let Some(requirement) = view.blocking_interaction.as_ref() else {
            return;
        };
        match self
            .definition
            .build_interaction(node.state.as_ref(), view, requirement)
        {
            Err(rejection) => self.violate(
                ExplorationViolationKind::BlockingInteractionNotBuildable { rejection },
                node,
            ),
            Ok(spec) => {
                if let Err(error) = spec.validate() {
                    self.violate(
                        ExplorationViolationKind::BlockingInteractionNotAnswerable { error },
                        node,
                    );
                }
            }
        }
    }

    /// Simulates every candidate command and returns the new frontier nodes.
    fn expand<M>(
        &mut self,
        model: &M,
        node: &Node<W::State>,
        facts: StateFacts,
    ) -> Vec<Node<W::State>>
    where
        M: WorkflowModel<W> + ?Sized,
    {
        let mut commands = model.candidate_commands(node.state.as_ref());
        if commands.len() > self.limits.max_commands_per_state {
            commands.truncate(self.limits.max_commands_per_state);
            self.truncated = true;
        }
        if commands.is_empty() && !facts.terminal && !facts.has_blocking_interaction {
            self.violate(ExplorationViolationKind::DeadEnd, node);
        }
        if node.depth >= self.limits.max_depth {
            self.truncated |= !commands.is_empty();
            return Vec::new();
        }
        let mut frontier = Vec::new();
        for command in &commands {
            self.transitions_simulated += 1;
            if let Some(next) = self.simulate_one(model, node, command) {
                frontier.push(next);
            }
        }
        frontier
    }

    fn simulate_one<M>(
        &mut self,
        model: &M,
        node: &Node<W::State>,
        command: &W::Command,
    ) -> Option<Node<W::State>>
    where
        M: WorkflowModel<W> + ?Sized,
    {
        let Ok(command_json) = canonical_value(command) else {
            self.violate(ExplorationViolationKind::UnserializableCommand, node);
            return None;
        };
        let command_type = command_label(&command_json);
        let before = canonical_value(&node.state).ok();
        let validated = self
            .definition
            .validate_command(node.state.as_ref(), command);
        let transition = model.simulate(node.state.as_ref(), command);
        let mutated = canonical_value(&node.state).ok() != before;
        match transition {
            SimulatedTransition::Rejected(_) => {
                if mutated {
                    self.violate(
                        ExplorationViolationKind::RejectedCommandMutatedState {
                            command: command_json,
                            command_type,
                        },
                        node,
                    );
                }
                None
            }
            SimulatedTransition::Applied { state, .. } => {
                if mutated {
                    self.violate(
                        ExplorationViolationKind::AppliedCommandMutatedInputState {
                            command: command_json.clone(),
                            command_type: command_type.clone(),
                        },
                        node,
                    );
                }
                // The projector half of this rule lives in
                // `check_absent_state_is_not_terminal`; it cannot see a removal,
                // because the absent state a removal produces is usually the
                // initial state and is therefore deduplicated away.
                if node.state.is_some() && state.is_none() {
                    self.violate(
                        ExplorationViolationKind::TransitionRemovesCase {
                            command: command_json.clone(),
                            command_type: command_type.clone(),
                        },
                        node,
                    );
                }
                if let Err(rejection) = validated {
                    self.violate(
                        ExplorationViolationKind::RejectedCommandApplied {
                            command: command_json.clone(),
                            command_type,
                            rejection,
                        },
                        node,
                    );
                }
                let mut path = node.path.clone();
                path.push(command_json);
                self.admit(state, node.depth + 1, path)
            }
        }
    }

    fn finish<M>(mut self, model: &M) -> ExplorationReport
    where
        M: WorkflowModel<W> + ?Sized,
    {
        // A truncated search proves nothing about what it did not visit.
        for outcome in if self.truncated {
            Vec::new()
        } else {
            model.declared_outcomes()
        } {
            let kind = match canonical_value(&outcome) {
                Ok(value) if self.reached_outcomes.contains(&value) => continue,
                Ok(outcome) => ExplorationViolationKind::UnreachableOutcome { outcome },
                Err(_) => ExplorationViolationKind::UnserializableState,
            };
            self.violations.push(ExplorationViolation {
                kind,
                state: serde_json::Value::Null,
                depth: 0,
                path: Vec::new(),
            });
        }
        ExplorationReport {
            workflow: self.definition.key(),
            workflow_version: self.definition.version(),
            limits: self.limits,
            states_explored: self.states_explored,
            transitions_simulated: self.transitions_simulated,
            max_depth_reached: self.max_depth_reached,
            truncated: self.truncated,
            reached_phases: self.reached_phases,
            reached_outcomes: self.reached_outcomes,
            violations: self.violations,
        }
    }
}

/// The first part of two erased views that differs, or `None` when they match.
///
/// The labels are field names, never values, so they are safe in a violation
/// message.
fn view_difference(
    left: &turnframe_core::flow::ErasedWorkflowView,
    right: &turnframe_core::flow::ErasedWorkflowView,
) -> Option<&'static str> {
    if left.phase != right.phase {
        return Some("phase");
    }
    if left.phase_ownership != right.phase_ownership {
        return Some("phase ownership");
    }
    if left.obligations.len() != right.obligations.len() {
        return Some("obligation count");
    }
    if left.obligations != right.obligations {
        return Some("obligations");
    }
    if left.blocking_interaction != right.blocking_interaction {
        return Some("blocking interaction");
    }
    if left.notices != right.notices {
        return Some("notices");
    }
    if left.outcome != right.outcome {
        return Some("outcome");
    }
    if left.workflow_version != right.workflow_version {
        return Some("workflow version");
    }
    None
}

/// Walks the model and returns every reachable state, in visit order.
///
/// This is the search without the projection: no definition is consulted and no
/// invariant is checked, so it is the tool for a domain-specific assertion that
/// [`ExplorationReport`](crate::explore::ExplorationReport) cannot express —
/// "some reachable trip really does carry four extras", "the second traveler
/// is reachable in more than one state". Deduplication and the limits work
/// exactly as in [`explore`], and the first element is an initial state.
#[must_use]
pub fn reachable_states<W, M>(model: &M, limits: ExplorationLimits) -> Vec<Option<W::State>>
where
    W: WorkflowDefinition,
    M: WorkflowModel<W> + ?Sized,
{
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<(Option<W::State>, usize)> = VecDeque::new();
    let mut visited: Vec<Option<W::State>> = Vec::new();
    let admit = |state: Option<W::State>,
                 depth: usize,
                 seen: &mut BTreeSet<String>,
                 queue: &mut VecDeque<(Option<W::State>, usize)>| {
        let Ok(key) = canonical_value(&state).map(|value| value.to_string()) else {
            return;
        };
        if seen.contains(&key) || seen.len() >= limits.max_states {
            return;
        }
        seen.insert(key);
        queue.push_back((state, depth));
    };
    for state in model.initial_states() {
        admit(state, 0, &mut seen, &mut queue);
    }
    while let Some((state, depth)) = queue.pop_front() {
        visited.push(state.clone());
        if depth >= limits.max_depth {
            continue;
        }
        let mut commands = model.candidate_commands(state.as_ref());
        commands.truncate(limits.max_commands_per_state);
        for command in &commands {
            if let SimulatedTransition::Applied { state: next, .. } =
                model.simulate(state.as_ref(), command)
            {
                admit(next, depth + 1, &mut seen, &mut queue);
            }
        }
    }
    visited
}

/// A stable label for a serialized command: the variant name for an externally
/// tagged enum, the string itself for a unit variant, `"command"` otherwise.
/// Never carries a value, so it is safe in `Display` output.
fn command_label(command: &serde_json::Value) -> String {
    match command {
        serde_json::Value::String(name) => name.clone(),
        serde_json::Value::Object(fields) => fields
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .or_else(|| fields.keys().next().map(String::as_str))
            .unwrap_or("command")
            .to_owned(),
        _ => "command".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::command_label;
    use serde_json::json;

    #[test]
    fn command_labels_never_carry_a_value() {
        assert_eq!(command_label(&json!("submit")), "submit");
        assert_eq!(
            command_label(&json!({"set_name": {"value": "x"}})),
            "set_name"
        );
        assert_eq!(
            command_label(&json!({"kind": "cancel", "reason": "x"})),
            "cancel"
        );
        assert_eq!(command_label(&json!(7)), "command");
        assert_eq!(command_label(&json!({})), "command");
    }
}
