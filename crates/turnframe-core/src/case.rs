//! Case references and versioned values (spec §7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{CaseId, CaseRevision, WorkflowKey};

/// Identity of a case without a revision: the key used to group views,
/// interactions and commands that belong to the same record.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct CaseKey {
    /// Workflow the case belongs to.
    pub workflow: WorkflowKey,
    /// Application-owned case identifier.
    pub case_id: CaseId,
}

impl CaseKey {
    /// Builds a key.
    #[must_use]
    pub fn new(workflow: impl Into<WorkflowKey>, case_id: impl Into<CaseId>) -> Self {
        Self {
            workflow: workflow.into(),
            case_id: case_id.into(),
        }
    }

    /// Attaches an expected revision, producing a [`CaseRef`].
    #[must_use]
    pub fn at(self, expected_revision: CaseRevision) -> CaseRef {
        CaseRef {
            workflow: self.workflow,
            case_id: self.case_id,
            expected_revision,
        }
    }
}

/// A reference to a case at an expected revision (spec §7, I13).
///
/// Every mutation names the revision it was planned against; a different
/// current revision is a conflict, never a blind overwrite.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CaseRef {
    /// Workflow the case belongs to.
    pub workflow: WorkflowKey,
    /// Application-owned case identifier.
    pub case_id: CaseId,
    /// Revision the caller believes is current.
    pub expected_revision: CaseRevision,
}

impl CaseRef {
    /// Builds a reference.
    #[must_use]
    pub fn new(
        workflow: impl Into<WorkflowKey>,
        case_id: impl Into<CaseId>,
        expected_revision: CaseRevision,
    ) -> Self {
        Self {
            workflow: workflow.into(),
            case_id: case_id.into(),
            expected_revision,
        }
    }

    /// The revision-less identity of the case.
    #[must_use]
    pub fn key(&self) -> CaseKey {
        CaseKey {
            workflow: self.workflow.clone(),
            case_id: self.case_id.clone(),
        }
    }

    /// Returns a copy pointing at another revision.
    #[must_use]
    pub fn with_revision(&self, revision: CaseRevision) -> Self {
        Self {
            workflow: self.workflow.clone(),
            case_id: self.case_id.clone(),
            expected_revision: revision,
        }
    }

    /// Returns `true` when both references name the same record, regardless of
    /// revision.
    #[must_use]
    pub fn same_case(&self, other: &CaseRef) -> bool {
        self.workflow == other.workflow && self.case_id == other.case_id
    }
}

/// A value read together with the case revision it was read at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Versioned<T> {
    /// The value.
    pub value: T,
    /// Revision of the case when the value was read.
    pub revision: CaseRevision,
}

impl<T> Versioned<T> {
    /// Pairs a value with its revision.
    #[must_use]
    pub const fn new(value: T, revision: CaseRevision) -> Self {
        Self { value, revision }
    }

    /// Borrows the inner value keeping the revision.
    #[must_use]
    pub const fn as_ref(&self) -> Versioned<&T> {
        Versioned {
            value: &self.value,
            revision: self.revision,
        }
    }

    /// Transforms the inner value keeping the revision.
    #[must_use]
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Versioned<U> {
        Versioned {
            value: f(self.value),
            revision: self.revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_ref_key_and_revision_helpers() {
        let r = CaseRef::new("trip", "trip-1", CaseRevision(4));
        assert_eq!(r.key(), CaseKey::new("trip", "trip-1"));
        assert_eq!(
            r.with_revision(CaseRevision(5)).expected_revision,
            CaseRevision(5)
        );
        assert!(r.same_case(&r.with_revision(CaseRevision(9))));
        assert_eq!(CaseKey::new("trip", "trip-1").at(CaseRevision(4)), r);
    }

    #[test]
    fn versioned_map_keeps_revision() {
        let v = Versioned::new(2_u32, CaseRevision(7)).map(|n| n * 2);
        assert_eq!(v.value, 4);
        assert_eq!(v.revision, CaseRevision(7));
    }
}
