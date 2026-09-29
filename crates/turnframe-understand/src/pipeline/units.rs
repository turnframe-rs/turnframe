//! From the segmentation's list to numbered units, and the units coverage adds.

use turnframe_core::ids::WorkflowKey;
use turnframe_core::plan::AnswerBasis;
use turnframe_core::understanding::{FoundBy, UnitId, UnitKind, WordRange};
use turnframe_tasks::TaskId;

use crate::input::UnderstandingInput;
use crate::tasks::coverage::{MissedKind, MissedUnit};
use crate::tasks::segment::{Segmentation, SegmentedUnit, UNKNOWN};
use crate::words::Span;

/// A unit and where it came from.
#[derive(Debug, Clone)]
pub(crate) struct Seg {
    pub id: UnitId,
    pub unit: SegmentedUnit,
    pub span: Span,
    pub range: WordRange,
    pub workflow: Option<WorkflowKey>,
    pub refers_to: Option<UnitId>,
    pub found_by: FoundBy,
    pub parent: TaskId,
    pub depth: u8,
}

impl Seg {
    pub fn kind(&self) -> UnitKind {
        self.unit.kind()
    }

    /// The unit read as a request for what it asks: a question whether a thing can be done.
    pub fn as_request(&self) -> Self {
        let mut request = self.clone();
        if let SegmentedUnit::Question {
            words, workflow, ..
        } = &self.unit
        {
            request.unit = SegmentedUnit::Request {
                words: *words,
                workflow: workflow.clone(),
            };
        }
        request
    }

    pub fn task(&self) -> TaskId {
        TaskId::new(self.id.to_string())
    }
}

fn workflow_of(name: Option<&str>) -> Option<WorkflowKey> {
    name.filter(|name| *name != UNKNOWN).map(WorkflowKey::from)
}

/// The segmentation's units in message order, numbered from 1.
pub(crate) fn numbered(
    turn: &UnderstandingInput,
    segmentation: &Segmentation,
    parent: &TaskId,
    depth: u8,
) -> Vec<Seg> {
    let mut order: Vec<usize> = (0..segmentation.units.len()).collect();
    order.sort_by_key(|&listed| segmentation.units[listed].words());
    let id_of_listed = |number: usize| {
        order
            .iter()
            .position(|&listed| listed + 1 == number)
            .map(|position| UnitId(u16::try_from(position + 1).unwrap_or(u16::MAX)))
    };
    order
        .iter()
        .enumerate()
        .filter_map(|(position, &listed)| {
            let unit = segmentation.units[listed].clone();
            let span = unit.words();
            let range = turn.message.range(span).ok()?;
            Some(Seg {
                id: UnitId(u16::try_from(position + 1).unwrap_or(u16::MAX)),
                workflow: workflow_of(unit.workflow()),
                // Only an earlier request or correction can be changed here; naming anything
                // else changes something from before this message.
                refers_to: unit
                    .refers_to()
                    .filter(|number| *number <= listed)
                    .filter(|number| {
                        matches!(
                            segmentation.units.get(number - 1),
                            Some(SegmentedUnit::Request { .. } | SegmentedUnit::Correction { .. })
                        )
                    })
                    .and_then(id_of_listed),
                unit,
                span,
                range,
                found_by: FoundBy::Segment,
                parent: parent.clone(),
                depth,
            })
        })
        .collect()
}

/// Why the segmentation is sent back once.
#[derive(Debug, Clone)]
pub(crate) enum Retry {
    /// Coverage found constraints in words no unit holds: their kind is unknown, so the
    /// turn cannot run as it was segmented.
    LostConstraint(Vec<Span>),
    /// Coverage read as an act words segmentation read as small talk or, when `dispute`,
    /// as a dispute.
    Disputed {
        words: Span,
        read_as: MissedKind,
        dispute: bool,
    },
}

/// What coverage added: new units, questions it lengthened, and units it read as an act
/// where segmentation read nothing to do, which run nothing.
#[derive(Debug, Default)]
pub(crate) struct Covered {
    pub added: Vec<UnitId>,
    pub extended: Vec<UnitId>,
    pub unclear: Vec<UnitId>,
}

/// Adds the units coverage found in words no unit covers, numbered after the others.
pub(crate) fn add_missed(
    turn: &UnderstandingInput,
    units: &mut Vec<Seg>,
    missed: &[MissedUnit],
    parent: &TaskId,
    depth: u8,
    reread_small_talk: bool,
    second_reading: bool,
) -> Result<Covered, Retry> {
    let mut covered = Covered::default();
    // A unit a question replaced still holds its other words for the findings after it.
    let mut replaced: Vec<Seg> = Vec::new();
    let mut lost: Vec<Span> = Vec::new();
    for found in missed {
        let span = found.words;
        let overlapping = |unit: &Seg| span.from <= unit.span.to && unit.span.from <= span.to;
        let Ok(range) = turn.message.range(span) else {
            continue;
        };
        // Words segmentation read as reaching nothing (small talk, a dispute) may become
        // a question, which is only answered. Two readings that disagree on whether they
        // ask for an act run nothing: the words are reported unclear.
        let replaceable =
            |unit: &Seg| matches!(unit.kind(), UnitKind::Chitchat | UnitKind::Dispute);
        let overlapped: Vec<&Seg> = units
            .iter()
            .chain(&replaced)
            .filter(|unit| overlapping(unit))
            .collect();
        // Words already read as a request stay that request: route lists everything it
        // asks for, so a second reading of them is not a second request.
        if !overlapped.is_empty() {
            if !overlapped.iter().all(|unit| replaceable(unit))
                || matches!(found.kind, MissedKind::Constraint)
            {
                continue;
            }
            if !matches!(found.kind, MissedKind::Question) {
                // A dispute read again after this check stands: it runs nothing either way.
                if second_reading
                    && overlapped
                        .iter()
                        .all(|unit| unit.kind() == UnitKind::Dispute)
                {
                    continue;
                }
                if reread_small_talk {
                    return Err(Retry::Disputed {
                        words: span,
                        read_as: found.kind,
                        dispute: overlapped
                            .iter()
                            .all(|unit| unit.kind() == UnitKind::Dispute),
                    });
                }
                covered.unclear.extend(
                    overlapped
                        .iter()
                        .filter(|unit| units.iter().any(|kept| kept.id == unit.id))
                        .map(|unit| unit.id),
                );
                continue;
            }
            let (gone, kept) = std::mem::take(units).into_iter().partition(overlapping);
            *units = kept;
            replaced.extend::<Vec<Seg>>(gone);
        }
        // A question found right after a question's words is its tail («…, right?»):
        // asked apart, each half would be answered without the other.
        if matches!(found.kind, MissedKind::Question)
            && let Some(asked) = units.iter_mut().find(|unit| {
                unit.kind() == UnitKind::Question && unit.span.to.saturating_add(1) == span.from
            })
            && let Ok(whole) = turn.message.range(Span::new(asked.span.from, span.to))
        {
            asked.span = Span::new(asked.span.from, span.to);
            asked.range = whole;
            covered.extended.push(asked.id);
            continue;
        }
        // A lone word between two parts is the word joining them: no unit of its own.
        if matches!(found.kind, MissedKind::Constraint | MissedKind::Request)
            && joins_two_parts(units, span)
        {
            continue;
        }
        let workflow = found.workflow.clone();
        let unit = match found.kind {
            MissedKind::Constraint => {
                lost.push(span);
                continue;
            }
            MissedKind::Request => SegmentedUnit::Request {
                words: span,
                workflow,
            },
            MissedKind::Question => SegmentedUnit::Question {
                words: span,
                workflow,
                basis: AnswerBasis::CurrentCommittedState,
                continues_previous: false,
            },
            MissedKind::Correction => SegmentedUnit::Correction {
                words: span,
                workflow,
                corrects: None,
            },
            MissedKind::Cancel => SegmentedUnit::Cancel {
                words: span,
                workflow,
                cancels: None,
            },
        };
        let next = units.iter().map(|unit| unit.id.0).max().unwrap_or(0);
        let id = UnitId(next.saturating_add(1));
        units.push(Seg {
            id,
            workflow: workflow_of(unit.workflow()),
            unit,
            span,
            range,
            refers_to: None,
            found_by: FoundBy::Coverage,
            parent: parent.clone(),
            depth,
        });
        covered.added.push(id);
    }
    if lost.is_empty() {
        Ok(covered)
    } else {
        Err(Retry::LostConstraint(lost))
    }
}

/// Words the first reading took as small talk, read again because a check read an act in
/// them: a condition the second reading makes of them is neither reading's, and a condition
/// holds every act of the turn, so they stay small talk.
pub(crate) fn small_talk_stays(units: &mut [Seg], words: Span) {
    for unit in units.iter_mut().filter(|unit| {
        unit.kind() == UnitKind::Constraint
            && words.from <= unit.span.from
            && unit.span.to <= words.to
    }) {
        unit.unit = SegmentedUnit::Chitchat { words: unit.span };
    }
}

/// Whether `span` is one word, with a unit ending right before it and one beginning right
/// after it.
fn joins_two_parts(units: &[Seg], span: Span) -> bool {
    span.from == span.to
        && units
            .iter()
            .any(|unit| unit.span.to.saturating_add(1) == span.from)
        && units
            .iter()
            .any(|unit| unit.span.from == span.to.saturating_add(1))
}

/// A request over `span`, which the whole-turn check found, numbered after the others.
pub(crate) fn found_by_check(
    turn: &UnderstandingInput,
    units: &[Seg],
    span: Span,
    parent: &TaskId,
) -> Option<Seg> {
    let range = turn.message.range(span).ok()?;
    let next = units.iter().map(|unit| unit.id.0).max().unwrap_or(0);
    Some(Seg {
        id: UnitId(next.saturating_add(1)),
        workflow: None,
        unit: SegmentedUnit::Request {
            words: span,
            workflow: UNKNOWN.to_owned(),
        },
        span,
        range,
        refers_to: None,
        found_by: FoundBy::CrossCheck,
        parent: parent.clone(),
        depth: 1,
    })
}

/// Turns into a question each request that `unrouted` says nothing on offer does, where
/// coverage read the same words as a question: two readings agree it asks something, and
/// a question can be answered where such a request can only be reported unread.
pub(crate) fn reread_as_questions(
    units: &mut [Seg],
    missed: &[MissedUnit],
    unrouted: impl Fn(UnitId) -> bool,
) -> Vec<UnitId> {
    let mut reread = Vec::new();
    for found in missed
        .iter()
        .filter(|found| found.kind == MissedKind::Question)
    {
        for unit in units.iter_mut() {
            let overlaps = found.words.from <= unit.span.to && unit.span.from <= found.words.to;
            if overlaps && unit.kind() == UnitKind::Request && unrouted(unit.id) {
                let workflow = unit.unit.workflow().unwrap_or(UNKNOWN).to_owned();
                unit.unit = SegmentedUnit::Question {
                    words: unit.span,
                    workflow,
                    basis: AnswerBasis::CurrentCommittedState,
                    continues_previous: false,
                };
                reread.push(unit.id);
            }
        }
    }
    reread
}

/// The questions that asked for something an operation does, read as the requests they are.
pub(crate) fn asked_for(units: &mut [Seg], asked: &[UnitId]) {
    for unit in units.iter_mut().filter(|unit| asked.contains(&unit.id)) {
        *unit = unit.as_request();
    }
}
