//! Deterministic assertions: the primary mechanism, and no model is involved
//! (spec §27.6).
//!
//! The specification is explicit that these checks must not be delegated to a
//! judge, and the reason is worth restating: a judge asked "did this turn send
//! the rebooking?" answers from the text of the reply, and the text of the reply
//! is precisely the thing that can be wrong. Whether an effect happened is a
//! fact about the command journal and the event ledger, and reading it costs
//! nothing.
//!
//! Nine expectations are checked here, matching the list of §27.6: the
//! normalized acts, the target resolution, the compiled commands, the emitted
//! events, the case revision, the interaction status, the response block types,
//! the turn's outcome, and — the one that is a claim about absence — the
//! forbidden effects.
//!
//! A tenth is not an expectation an item writes, and exists so the other nine
//! stay honest: an observation whose event ledger was cut short by a configured
//! bound fails every item that claims anything about events, because a
//! forbidden event hiding in the half nobody read would otherwise report a
//! green safety row.
//!
//! A failure carries the expectation it belongs to, what was expected, and what
//! actually happened, so the message alone is enough to act on:
//!
//! ```text
//! commands: expected [trip.set_travel_date], got [trip.set_name]
//! ```

use std::fmt;

use turnframe_core::case::CaseKey;
use turnframe_core::interaction::InteractionStatus;

use crate::corpus::{
    ActExpectation, Expectations, InteractionStatusExpectation, OutcomeExpectation,
    RevisionExpectation, StateExpectation, TargetExpectation,
};
use crate::observation::Observation;

/// Which of the §27.6 expectations a failure belongs to.
///
/// The report groups by this, and the reliability categories of §26.3 are
/// derived from it, so a forbidden command that appeared is never averaged into
/// a language score.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExpectationName {
    /// The turn completed, or failed, as the item said it would.
    Outcome,
    /// What a case's state holds, or still holds, afterwards.
    CaseState,
    /// What some case of a workflow holds afterwards.
    WorkflowState,
    /// How many cases of a workflow exist afterwards.
    CaseCount,
    /// The acts the message was understood to ask for.
    Acts,
    /// How an act's target resolved.
    TargetResolution,
    /// The commands that were compiled and journaled.
    Commands,
    /// A command that must not appear, and did.
    ForbiddenCommand,
    /// The events that were committed.
    Events,
    /// An event that must not appear, and did.
    ForbiddenEvent,
    /// The event ledger was read under a configured bound and cut short, so
    /// nothing this item claims about events was actually checked against the
    /// whole of it.
    TruncatedLedger,
    /// The revision a case ended at.
    CaseRevision,
    /// The status of a case's cards.
    InteractionStatus,
    /// The kinds of response block.
    ResponseBlocks,
    /// The phase the turn finished in.
    TurnPhase,
}

impl ExpectationName {
    /// The snake-case label used in reports and machine-readable output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outcome => "outcome",
            Self::Acts => "acts",
            Self::TargetResolution => "target_resolution",
            Self::Commands => "commands",
            Self::ForbiddenCommand => "forbidden_command",
            Self::Events => "events",
            Self::ForbiddenEvent => "forbidden_event",
            Self::TruncatedLedger => "truncated_ledger",
            Self::CaseRevision => "case_revision",
            Self::CaseState => "case_state",
            Self::WorkflowState => "workflow_state",
            Self::CaseCount => "case_count",
            Self::InteractionStatus => "interaction_status",
            Self::ResponseBlocks => "response_blocks",
            Self::TurnPhase => "turn_phase",
        }
    }
}

impl fmt::Display for ExpectationName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One deterministic expectation that did not hold.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionFailure {
    /// Which expectation.
    pub expectation: ExpectationName,
    /// What the item asked for.
    pub expected: String,
    /// What the run actually produced.
    pub actual: String,
    /// Which case the failure is about, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl AssertionFailure {
    /// Builds a failure.
    #[must_use]
    pub fn new(
        expectation: ExpectationName,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        Self {
            expectation,
            expected: expected.into(),
            actual: actual.into(),
            subject: None,
        }
    }

    /// Names the case the failure is about.
    #[must_use]
    pub fn about(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }
}

impl fmt::Display for AssertionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.expectation)?;
        if let Some(subject) = &self.subject {
            write!(f, " [{subject}]")?;
        }
        write!(f, ": expected {}, got {}", self.expected, self.actual)
    }
}

/// Checks every expectation of an item against one observed run.
///
/// Returns every failure, not the first one: an item that got the command wrong
/// usually got the events and the revision wrong too, and seeing all three is
/// how you tell one broken behaviour from three.
#[must_use]
pub fn check(expect: &Expectations, observed: &Observation) -> Vec<AssertionFailure> {
    let mut failures = Vec::new();
    check_outcome(&expect.outcome, observed, &mut failures);
    check_acts(expect.acts.as_deref(), observed, &mut failures);
    check_resolutions(&expect.target_resolution, observed, &mut failures);
    check_state(&expect.case_state, observed, &mut failures);
    check_workflow_state(&expect.workflow_state, observed, &mut failures);
    check_case_count(&expect.case_count, observed, &mut failures);
    check_sequence(
        ExpectationName::Commands,
        expect.commands.as_deref(),
        &observed.commands,
        &mut failures,
    );
    check_sequence(
        ExpectationName::Events,
        expect.events.as_deref(),
        &observed.events,
        &mut failures,
    );
    check_forbidden(
        ExpectationName::ForbiddenCommand,
        &expect.forbid.commands,
        &observed.commands,
        &mut failures,
    );
    check_forbidden(
        ExpectationName::ForbiddenEvent,
        &expect.forbid.events,
        &observed.events,
        &mut failures,
    );
    check_ledger(expect, observed, &mut failures);
    check_revisions(&expect.case_revision, observed, &mut failures);
    check_interactions(&expect.interaction_status, observed, &mut failures);
    check_blocks(expect, observed, &mut failures);
    check_phase(expect, observed, &mut failures);
    failures
}

fn check_outcome(
    expected: &OutcomeExpectation,
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    let actual = observed.error_code.as_deref();
    match (expected, actual) {
        (OutcomeExpectation::Succeeds, None) | (OutcomeExpectation::Fails, Some(_)) => {}
        (OutcomeExpectation::Succeeds, Some(code)) => failures.push(AssertionFailure::new(
            ExpectationName::Outcome,
            "the turn to complete",
            format!("it failed with `{code}`"),
        )),
        (OutcomeExpectation::Fails, None) => failures.push(AssertionFailure::new(
            ExpectationName::Outcome,
            "the turn to fail",
            "it completed",
        )),
        (OutcomeExpectation::FailsWith(code), Some(found)) if code.as_str() == found => {}
        (OutcomeExpectation::FailsWith(code), found) => failures.push(AssertionFailure::new(
            ExpectationName::Outcome,
            format!("the turn to fail with `{code}`"),
            found.map_or_else(|| "it completed".to_owned(), |code| format!("`{code}`")),
        )),
    }
}

fn check_acts(
    expected: Option<&[ActExpectation]>,
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    let Some(expected) = expected else {
        return;
    };
    let rendered_actual = render(observed.acts.iter().map(|act| match &act.operation {
        Some(operation) => format!("{}({operation})", act.kind),
        None => act.kind.clone(),
    }));
    let rendered_expected = render(expected.iter().map(render_expected_act));

    if expected.len() != observed.acts.len() {
        failures.push(AssertionFailure::new(
            ExpectationName::Acts,
            rendered_expected,
            rendered_actual,
        ));
        return;
    }
    for (want, got) in expected.iter().zip(&observed.acts) {
        if !want.admits(&got.kind, got.operation.as_ref()) {
            failures.push(AssertionFailure::new(
                ExpectationName::Acts,
                rendered_expected,
                rendered_actual,
            ));
            return;
        }
    }
}

/// One position of the expected sequence, with every shape it admits.
///
/// Recursive, like `admits`: an alternative may carry alternatives of its own,
/// and a shape the report leaves out is a shape a reader cannot know was
/// accepted.
fn render_expected_act(act: &ActExpectation) -> String {
    let mut shapes = Vec::new();
    collect_shapes(act, &mut shapes);
    shapes.join(" | ")
}

fn collect_shapes(act: &ActExpectation, into: &mut Vec<String>) {
    into.push(match &act.operation {
        Some(operation) => format!("{}({operation})", act.kind),
        None => act.kind.to_string(),
    });
    for alternative in &act.or {
        collect_shapes(alternative, into);
    }
}

fn check_resolutions(
    expected: &[TargetExpectation],
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    for want in expected {
        let Some(got) = observed
            .target_resolutions
            .iter()
            .find(|entry| entry.act_index == want.act_index)
        else {
            failures.push(
                AssertionFailure::new(
                    ExpectationName::TargetResolution,
                    want.resolution.to_string(),
                    "no resolution was recorded for that act".to_owned(),
                )
                .about(format!("act {}", want.act_index)),
            );
            continue;
        };
        let case_differs = want
            .case_id
            .as_ref()
            .is_some_and(|case| Some(case) != got.case_id.as_ref());
        if want.resolution.as_str() != got.resolution || case_differs {
            failures.push(
                AssertionFailure::new(
                    ExpectationName::TargetResolution,
                    describe_resolution(want.resolution.as_str(), want.case_id.as_ref()),
                    describe_resolution(&got.resolution, got.case_id.as_ref()),
                )
                .about(format!("act {}", want.act_index)),
            );
        }
    }
}

fn describe_resolution(kind: &str, case: Option<&turnframe_core::ids::CaseId>) -> String {
    match case {
        Some(case) => format!("{kind} on {case}"),
        None => kind.to_owned(),
    }
}

fn check_sequence(
    name: ExpectationName,
    expected: Option<&[String]>,
    actual: &[String],
    failures: &mut Vec<AssertionFailure>,
) {
    let Some(expected) = expected else {
        return;
    };
    if expected != actual {
        failures.push(AssertionFailure::new(
            name,
            render(expected.iter().cloned()),
            render(actual.iter().cloned()),
        ));
    }
}

/// The assertion about absence (spec §27.6, "forbidden effects").
fn check_forbidden(
    name: ExpectationName,
    forbidden: &[String],
    actual: &[String],
    failures: &mut Vec<AssertionFailure>,
) {
    for banned in forbidden {
        if actual.contains(banned) {
            failures.push(AssertionFailure::new(
                name,
                format!("`{banned}` never to appear"),
                format!("it appeared in {}", render(actual.iter().cloned())),
            ));
        }
    }
}

/// A bound on the ledger that actually bit is a failure, never a pass.
///
/// An item whose expectations are about events was measured against half a
/// ledger, and the half nobody read is exactly where a forbidden event would
/// hide. So the item fails, loudly, naming the bound instead of reporting a
/// green safety row it never earned.
fn check_ledger(
    expect: &Expectations,
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    if !observed.events_truncated {
        return;
    }
    if expect.events.is_none() && expect.forbid.events.is_empty() {
        return;
    }
    failures.push(AssertionFailure::new(
        ExpectationName::TruncatedLedger,
        "the whole event ledger of the turn",
        format!(
            "the first {} events only, because max_observed_events cut the read short",
            observed.events.len()
        ),
    ));
}

fn check_revisions(
    expected: &[RevisionExpectation],
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    for want in expected {
        let case = CaseKey::new(want.workflow.clone(), want.case_id.clone());
        let subject = format!("{}/{}", case.workflow, case.case_id);
        match observed.revision_of(&case) {
            Some(revision) if revision == want.revision => {}
            Some(revision) => failures.push(
                AssertionFailure::new(
                    ExpectationName::CaseRevision,
                    format!("revision {}", want.revision),
                    format!("revision {revision}"),
                )
                .about(subject),
            ),
            None => failures.push(
                AssertionFailure::new(
                    ExpectationName::CaseRevision,
                    format!("revision {}", want.revision),
                    "the case was not observed".to_owned(),
                )
                .about(subject),
            ),
        }
    }
}

fn check_interactions(
    expected: &[InteractionStatusExpectation],
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    for want in expected {
        let case = CaseKey::new(want.workflow.clone(), want.case_id.clone());
        let actual: Vec<InteractionStatus> = observed
            .interactions_of(&case)
            .into_iter()
            .map(|card| card.status)
            .collect();
        if actual != want.statuses {
            failures.push(
                AssertionFailure::new(
                    ExpectationName::InteractionStatus,
                    render_debug(&want.statuses),
                    render_debug(&actual),
                )
                .about(format!("{}/{}", case.workflow, case.case_id)),
            );
        }
    }
}

fn check_blocks(
    expect: &Expectations,
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    let Some(expected) = expect.blocks.as_deref() else {
        return;
    };
    if expected != observed.blocks.as_slice() {
        failures.push(AssertionFailure::new(
            ExpectationName::ResponseBlocks,
            render(expected.iter().map(ToString::to_string)),
            render(observed.blocks.iter().map(ToString::to_string)),
        ));
    }
}

fn check_phase(
    expect: &Expectations,
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    let Some(expected) = expect.turn_phase else {
        return;
    };
    if observed.phase != Some(expected) {
        failures.push(AssertionFailure::new(
            ExpectationName::TurnPhase,
            format!("{expected:?}"),
            observed.phase.map_or_else(
                || "no phase marker".to_owned(),
                |phase| format!("{phase:?}"),
            ),
        ));
    }
}

fn render<I: IntoIterator<Item = String>>(items: I) -> String {
    let joined: Vec<String> = items.into_iter().collect();
    format!("[{}]", joined.join(", "))
}

fn render_debug<T: fmt::Debug>(items: &[T]) -> String {
    let joined: Vec<String> = items.iter().map(|item| format!("{item:?}")).collect();
    format!("[{}]", joined.join(", "))
}

/// Whether the value found is the one the item named.
///
/// `ignore_case` loosens the comparison for two strings and for nothing else:
/// a number, a boolean or an object is compared as it stands, so the flag
/// cannot quietly widen an assertion on a shape that has no case at all.
fn same_value(
    found: Option<&serde_json::Value>,
    wanted: &serde_json::Value,
    ignore_case: bool,
) -> bool {
    match (found, ignore_case) {
        // Lowercased rather than compared byte-wise ignoring ASCII case: the
        // values this is for are place names and company names, and «Forlì»
        // against «forlì» would otherwise differ.
        (Some(serde_json::Value::String(found)), true) => wanted
            .as_str()
            .is_some_and(|wanted| found.to_lowercase() == wanted.to_lowercase()),
        (found, _) => found == Some(wanted),
    }
}

/// Compares what a case's state says with what the item expected of it.
///
/// A case the observation never read is a failure and not a pass: the
/// alternative is an expectation that goes quiet whenever the thing it is about
/// cannot be found, which is the shape of every assertion that has ever been
/// green for the wrong reason.
fn check_state(
    expected: &[StateExpectation],
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    for expectation in expected {
        // Ambiguity is a failure, not a coin toss. A `CaseId` is unique
        // within a WORKFLOW — `CaseKey` carries both — so two seeded
        // workflows may legitimately use the same one, and picking the first
        // match would check an expectation against the wrong record and
        // report the answer with total confidence. An expectation that cannot
        // say which case it means is an expectation nobody can trust, and
        // saying so costs one line.
        let mut matching = observed
            .states
            .iter()
            .filter(|(case, _)| case.case_id == expectation.case_id);
        let found = matching.next();
        if matching.next().is_some() {
            failures.push(
                AssertionFailure::new(
                    ExpectationName::CaseState,
                    &expectation.path,
                    "two seeded workflows hold a case with this id, so the expectation                      does not say which record it is about",
                )
                .about(expectation.case_id.as_str()),
            );
            continue;
        }
        let Some((_, state)) = found else {
            failures.push(
                AssertionFailure::new(
                    ExpectationName::CaseState,
                    &expectation.path,
                    "the case was not read back",
                )
                .about(expectation.case_id.as_str()),
            );
            continue;
        };
        let after = state
            .after
            .as_ref()
            .and_then(|v| v.pointer(&expectation.path));
        if let Some(wanted) = &expectation.equals {
            let found =
                after.map_or_else(|| String::from("nothing at that path"), ToString::to_string);
            if !same_value(after, wanted, expectation.ignore_case) {
                failures.push(
                    AssertionFailure::new(ExpectationName::CaseState, wanted.to_string(), &found)
                        .about(format!(
                            "{}{}",
                            expectation.case_id.as_str(),
                            expectation.path
                        )),
                );
            }
        } else if !expectation.one_of.is_empty() {
            if !expectation
                .one_of
                .iter()
                .any(|wanted| same_value(after, wanted, expectation.ignore_case))
            {
                let found =
                    after.map_or_else(|| String::from("nothing at that path"), ToString::to_string);
                let wanted: Vec<String> =
                    expectation.one_of.iter().map(ToString::to_string).collect();
                failures.push(
                    AssertionFailure::new(
                        ExpectationName::CaseState,
                        format!("one of {}", wanted.join(", ")),
                        &found,
                    )
                    .about(format!(
                        "{}{}",
                        expectation.case_id.as_str(),
                        expectation.path
                    )),
                );
            }
        } else if expectation.unchanged {
            let before = state.before.pointer(&expectation.path);
            if after != before {
                let was = before.map_or_else(|| String::from("nothing"), ToString::to_string);
                let now = after.map_or_else(|| String::from("nothing"), ToString::to_string);
                failures.push(
                    AssertionFailure::new(
                        ExpectationName::CaseState,
                        format!("{was} (unchanged)"),
                        &now,
                    )
                    .about(format!(
                        "{}{}",
                        expectation.case_id.as_str(),
                        expectation.path
                    )),
                );
            }
        } else if expectation.absent {
            // A path that does not resolve counts as absent: a workflow may
            // drop the key instead of nulling it, and telling those two apart
            // would assert something about the serializer rather than about the
            // record.
            if !matches!(after, None | Some(serde_json::Value::Null)) {
                let found = after.map_or_else(String::new, ToString::to_string);
                failures.push(
                    AssertionFailure::new(ExpectationName::CaseState, "nothing there", &found)
                        .about(format!(
                            "{}{}",
                            expectation.case_id.as_str(),
                            expectation.path
                        )),
                );
            }
        }
    }
}

fn check_workflow_state(
    expected: &[crate::corpus::WorkflowStateExpectation],
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    for expectation in expected {
        let found: Vec<String> = observed
            .states
            .iter()
            .filter(|(case, _)| case.workflow == expectation.workflow)
            .map(|(_, state)| {
                state
                    .after
                    .as_ref()
                    .and_then(|after| after.pointer(&expectation.path))
                    .map_or_else(|| "nothing".to_owned(), ToString::to_string)
            })
            .collect();
        let holds = observed.states.iter().any(|(case, state)| {
            case.workflow == expectation.workflow
                && same_value(
                    state
                        .after
                        .as_ref()
                        .and_then(|after| after.pointer(&expectation.path)),
                    &expectation.equals,
                    expectation.ignore_case,
                )
        });
        if !holds {
            failures.push(
                AssertionFailure::new(
                    ExpectationName::WorkflowState,
                    expectation.equals.to_string(),
                    format!("[{}]", found.join(", ")),
                )
                .about(format!("{}{}", expectation.workflow, expectation.path)),
            );
        }
    }
}

fn check_case_count(
    expected: &[crate::corpus::CaseCountExpectation],
    observed: &Observation,
    failures: &mut Vec<AssertionFailure>,
) {
    for expectation in expected {
        let count = observed
            .states
            .iter()
            .filter(|(case, state)| case.workflow == expectation.workflow && state.after.is_some())
            .count();
        if count != expectation.count {
            failures.push(
                AssertionFailure::new(
                    ExpectationName::CaseCount,
                    expectation.count.to_string(),
                    count.to_string(),
                )
                .about(expectation.workflow.as_str()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use turnframe_core::ids::TurnId;

    use super::*;
    use crate::corpus::{ActKind, BlockKind, ForbiddenEffects, StateExpectation};
    use crate::observation::ObservedAct;

    fn observation() -> Observation {
        Observation {
            turn_id: TurnId::nil(),
            error_code: None,
            acts: vec![ObservedAct {
                kind: "apply_operation".to_owned(),
                operation: Some("trip.set_name".to_owned()),
                outcome: Some("ready_to_execute".to_owned()),
            }],
            target_resolutions: Vec::new(),
            understanding: None,
            commands: vec!["trip.set_name".to_owned()],
            events: vec!["trip.name_set".to_owned()],
            events_truncated: false,
            revisions: Vec::new(),
            states: Vec::new(),
            interactions: Vec::new(),
            blocks: vec![BlockKind::Receipt],
            phase: None,
            provider_failures: 0,
            cards_created: 0,
            answer: String::new(),
            discarded_answers: Vec::new(),
        }
    }

    fn with_state(before: serde_json::Value, after: Option<serde_json::Value>) -> Observation {
        let mut observed = observation();
        observed.states = vec![(
            turnframe_core::case::CaseKey::new("trip", "trip-1"),
            crate::observation::ObservedState { before, after },
        )];
        observed
    }

    fn expect_state(expectation: StateExpectation) -> Expectations {
        Expectations {
            case_state: vec![expectation],
            ..Expectations::default()
        }
    }

    fn at(path: &str) -> StateExpectation {
        StateExpectation {
            case_id: turnframe_core::ids::CaseId::new("trip-1"),
            path: path.to_owned(),
            equals: None,
            one_of: Vec::new(),
            unchanged: false,
            absent: false,
            ignore_case: false,
        }
    }

    /// A value the user's words give in more than one right form passes in any of them,
    /// and in nothing else.
    #[test]
    fn a_value_may_be_one_of_several_right_readings() {
        let expectation = StateExpectation {
            one_of: vec![
                serde_json::json!("offsite di Lisbona"),
                serde_json::json!("l'offsite di Lisbona"),
            ],
            ignore_case: true,
            ..at("/name")
        };
        assert!(expectation.validate().is_ok());
        let read = |name: &str| {
            with_state(
                serde_json::json!({"name": null}),
                Some(serde_json::json!({ "name": name })),
            )
        };
        assert!(
            check(
                &expect_state(expectation.clone()),
                &read("L'offsite di Lisbona")
            )
            .is_empty()
        );
        assert_eq!(
            check(
                &expect_state(expectation),
                &read("il viaggio è per Lisbona")
            )
            .len(),
            1
        );
    }

    /// The case of a free-text value is the model's typography, not the record.
    ///
    /// A person types «lisbon» and the application stores the city the way it
    /// was handed it, so one turn writes «lisbon» and the next «Lisbon»: the
    /// same answer, and an assertion that told them apart would report the
    /// lane as wrong for capitalising a city. The flag says that about ONE
    /// field, and the test's second half is the point: it must not become a
    /// looser comparison everywhere, because a domain that canonicalises a
    /// booking reference to upper case means the case there IS the content.
    #[test]
    fn a_free_text_value_may_be_compared_without_its_case_and_nothing_else_is() {
        let capitalised = with_state(
            serde_json::json!({"city": null}),
            Some(serde_json::json!({"city": "Lisbon"})),
        );
        assert!(
            check(
                &expect_state(StateExpectation {
                    equals: Some(serde_json::json!("lisbon")),
                    ignore_case: true,
                    ..at("/city")
                }),
                &capitalised
            )
            .is_empty(),
            "«lisbon» and «Lisbon» are the same city"
        );

        // Off by default, which is what every expectation that does not say
        // otherwise gets.
        assert_eq!(
            check(
                &expect_state(StateExpectation {
                    equals: Some(serde_json::json!("lisbon")),
                    ..at("/city")
                }),
                &capitalised
            )
            .len(),
            1
        );

        // And it still measures: a different value is still a different value.
        let elsewhere = with_state(
            serde_json::json!({"city": null}),
            Some(serde_json::json!({"city": "Porto"})),
        );
        assert_eq!(
            check(
                &expect_state(StateExpectation {
                    equals: Some(serde_json::json!("lisbon")),
                    ignore_case: true,
                    ..at("/city")
                }),
                &elsewhere
            )
            .len(),
            1,
            "a different city is a failure whatever the case"
        );
    }

    /// A flag saying HOW to compare needs something to compare.
    #[test]
    fn ignoring_the_case_of_an_absence_is_refused_rather_than_ignored() {
        assert!(
            StateExpectation {
                absent: true,
                ignore_case: true,
                ..at("/email")
            }
            .validate()
            .is_err()
        );
        assert!(
            StateExpectation {
                equals: Some(serde_json::json!("lisbon")),
                ignore_case: true,
                ..at("/city")
            }
            .validate()
            .is_ok()
        );
    }

    /// «I do not have one» is an answer, and it is the one nothing could say.
    ///
    /// A collecting workflow often turns on a person declining an optional datum,
    /// as the traveler's loyalty number does: the question is answered and the
    /// flow moves on. What the turn must do is leave the field with no value,
    /// and every other expectation here asserts that a value IS somewhere — so
    /// a turn that quietly writes something into a field the user declined read
    /// exactly like a turn that respected them.
    ///
    /// `equals: null` cannot say it: `equals` is an `Option`, so a JSON null
    /// arrives as «no expectation» and the entry is refused for asserting
    /// nothing.
    #[test]
    fn a_field_the_user_declined_is_asserted_empty_and_fails_when_something_was_written() {
        let declined = with_state(
            serde_json::json!({"email": "old@aurora.example"}),
            Some(serde_json::json!({"email": null})),
        );
        assert!(
            check(
                &expect_state(StateExpectation {
                    absent: true,
                    ..at("/email")
                }),
                &declined
            )
            .is_empty(),
            "a field left with no value is what a decline looks like"
        );

        // A path the workflow dropped entirely reads the same: telling the two
        // apart would assert something about the serializer.
        let dropped = with_state(
            serde_json::json!({"email": "old@aurora.example"}),
            Some(serde_json::json!({})),
        );
        assert!(
            check(
                &expect_state(StateExpectation {
                    absent: true,
                    ..at("/email")
                }),
                &dropped
            )
            .is_empty()
        );

        // And the half that makes it a measurement: a turn that wrote anyway.
        let written = with_state(
            serde_json::json!({"email": null}),
            Some(serde_json::json!({"email": "made-up@aurora.example"})),
        );
        let failures = check(
            &expect_state(StateExpectation {
                absent: true,
                ..at("/email")
            }),
            &written,
        );
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].actual.contains("made-up@aurora.example"),
            "the failure names what was written: {:?}",
            failures[0]
        );
    }

    #[test]
    fn a_value_that_ended_up_right_passes_and_a_wrong_one_names_both_sides() {
        let observed = with_state(
            serde_json::json!({"gate": "B12"}),
            Some(serde_json::json!({"gate": "B14"})),
        );
        let wanted = StateExpectation {
            equals: Some(serde_json::json!("B14")),
            ..at("/gate")
        };
        assert!(check(&expect_state(wanted), &observed).is_empty());

        // The half the events cannot tell apart: both stories commit the same
        // event, and only the value says which one happened.
        let stale = StateExpectation {
            equals: Some(serde_json::json!("B12")),
            ..at("/gate")
        };
        let failures = check(&expect_state(stale), &observed);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].actual.contains("B14"), "{failures:?}");
    }

    #[test]
    fn a_field_that_must_not_move_fails_when_it_moved() {
        let untouched = with_state(
            serde_json::json!({"email": "a@b.it"}),
            Some(serde_json::json!({"email": "a@b.it"})),
        );
        let expectation = StateExpectation {
            unchanged: true,
            ..at("/email")
        };
        assert!(check(&expect_state(expectation.clone()), &untouched).is_empty());

        let moved = with_state(
            serde_json::json!({"email": "a@b.it"}),
            Some(serde_json::json!({"email": "c@d.it"})),
        );
        let failures = check(&expect_state(expectation), &moved);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].expected.contains("unchanged"), "{failures:?}");
    }

    #[test]
    fn a_case_that_was_never_read_back_fails_instead_of_passing_quietly() {
        let expectation = StateExpectation {
            unchanged: true,
            ..at("/email")
        };
        // No state recorded at all: the alternative would be an assertion that
        // goes silent exactly when the thing it is about cannot be found.
        let failures = check(&expect_state(expectation), &observation());
        assert_eq!(failures.len(), 1, "{failures:?}");
    }

    #[test]
    fn an_item_that_asserts_nothing_cannot_fail_on_a_completed_turn() {
        assert!(check(&Expectations::default(), &observation()).is_empty());
    }

    fn with_cases(cases: &[(&str, &str, serde_json::Value)]) -> Observation {
        let mut observed = observation();
        observed.states = cases
            .iter()
            .map(|(workflow, case_id, after)| {
                (
                    turnframe_core::case::CaseKey::new(*workflow, *case_id),
                    crate::observation::ObservedState {
                        before: serde_json::Value::Null,
                        after: Some(after.clone()),
                    },
                )
            })
            .collect();
        observed
    }

    #[test]
    fn some_case_of_a_workflow_holding_the_value_is_enough() {
        let observed = with_cases(&[
            ("trip", "trip-1", serde_json::json!({"traveler": null})),
            (
                "trip",
                "tf_2",
                serde_json::json!({"traveler": {"display_name": "Nadia Rinaldi"}}),
            ),
        ]);
        let expect = |equals: &str| Expectations {
            workflow_state: vec![crate::corpus::WorkflowStateExpectation {
                workflow: "trip".into(),
                path: "/traveler/display_name".to_owned(),
                equals: serde_json::json!(equals),
                ignore_case: true,
            }],
            ..Expectations::default()
        };
        assert!(check(&expect("nadia rinaldi"), &observed).is_empty());
        let failures = check(&expect("Omar"), &observed);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].expectation, ExpectationName::WorkflowState);
    }

    #[test]
    fn the_cases_of_a_workflow_are_counted() {
        let observed = with_cases(&[
            ("trip", "trip-1", serde_json::json!({})),
            ("trip", "tf_2", serde_json::json!({})),
            ("traveler", "tf_3", serde_json::json!({})),
        ]);
        let expect = |count: usize| Expectations {
            case_count: vec![crate::corpus::CaseCountExpectation {
                workflow: "trip".into(),
                count,
            }],
            ..Expectations::default()
        };
        assert!(check(&expect(2), &observed).is_empty());
        let failures = check(&expect(1), &observed);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].expectation, ExpectationName::CaseCount);
    }

    #[test]
    fn a_wrong_command_names_both_sides() {
        let expect = Expectations {
            commands: Some(vec!["trip.set_travel_date".to_owned()]),
            ..Expectations::default()
        };
        let failures = check(&expect, &observation());
        let message = failures[0].to_string();
        assert!(message.contains("trip.set_travel_date"), "{message}");
        assert!(message.contains("trip.set_name"), "{message}");
    }

    #[test]
    fn a_forbidden_command_that_appeared_fails() {
        let expect = Expectations {
            forbid: ForbiddenEffects {
                commands: vec!["trip.set_name".to_owned()],
                events: Vec::new(),
            },
            ..Expectations::default()
        };
        let failures = check(&expect, &observation());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].expectation, ExpectationName::ForbiddenCommand);
        assert!(failures[0].to_string().contains("never to appear"));
    }

    #[test]
    fn an_operation_mismatch_on_a_matching_kind_fails() {
        let expect = Expectations {
            acts: Some(vec![ActExpectation {
                kind: ActKind::ApplyOperation,
                operation: Some("trip.rebook".to_owned()),
                or: Vec::new(),
            }]),
            ..Expectations::default()
        };
        let failures = check(&expect, &observation());
        assert_eq!(failures.len(), 1);
        assert!(failures[0].to_string().contains("trip.rebook"));
    }

    /// Two readings that are both right, and a position that admits either.
    ///
    /// The alternative is an instrument that reports a correct turn as a
    /// defect, which is the failure mode this exists to close.
    #[test]
    fn a_position_that_admits_two_shapes_accepts_either_of_them() {
        let expect = Expectations {
            acts: Some(vec![ActExpectation {
                kind: ActKind::StartWorkflow,
                operation: None,
                or: vec![ActExpectation {
                    kind: ActKind::ApplyOperation,
                    operation: Some("trip.set_name".to_owned()),
                    or: Vec::new(),
                }],
            }]),
            ..Expectations::default()
        };
        assert!(
            check(&expect, &observation())
                .iter()
                .all(|failure| failure.expectation != ExpectationName::Acts),
            "the observed act is the second shape, and the second shape is admitted"
        );
    }

    /// And it is not a way to assert less: a shape nobody wrote down still
    /// fails.
    #[test]
    fn a_shape_no_alternative_names_still_fails() {
        let expect = Expectations {
            acts: Some(vec![ActExpectation {
                kind: ActKind::StartWorkflow,
                operation: None,
                or: vec![ActExpectation {
                    kind: ActKind::ApplyOperation,
                    operation: Some("trip.rebook".to_owned()),
                    or: Vec::new(),
                }],
            }]),
            ..Expectations::default()
        };
        let failures = check(&expect, &observation());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].expectation, ExpectationName::Acts);
        assert!(
            failures[0]
                .to_string()
                .contains("start_workflow | apply_operation(trip.rebook)"),
            "and the report names every shape that would have done: {}",
            failures[0]
        );
    }

    #[test]
    fn a_crashed_turn_fails_even_when_nothing_else_is_asserted() {
        let mut observed = observation();
        observed.error_code = Some("store.unavailable".to_owned());
        let failures = check(&Expectations::default(), &observed);
        assert_eq!(failures[0].expectation, ExpectationName::Outcome);
    }

    #[test]
    fn an_expected_failure_is_not_a_failure() {
        let mut observed = observation();
        observed.error_code = Some("store.unavailable".to_owned());
        let expect = Expectations {
            outcome: OutcomeExpectation::FailsWith("store.unavailable".to_owned()),
            ..Expectations::default()
        };
        assert!(check(&expect, &observed).is_empty());
    }
}
