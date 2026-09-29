//! The domain's own check of an understood act, run before the plan exists.

use turnframe_core::error::DomainRejection;
use turnframe_core::understanding::UnderstoodAct;

/// Dry-runs an act against its record: the domain's pure compile and validation.
///
/// A rejection naming an argument gets one repair of the extraction with its
/// explanation, then the act asks for that argument. A rejection naming none is left
/// for the reducer, which refuses the act as it does any other.
pub trait ActChecker: Send + Sync {
    /// Checks `act`.
    ///
    /// # Errors
    ///
    /// The domain's rejection.
    fn check(&self, act: &UnderstoodAct) -> Result<(), DomainRejection>;

    /// Whether it checks anything; one that does not is skipped, and no step says it ran.
    fn is_active(&self) -> bool {
        true
    }
}

/// Checks nothing: every act passes to the reducer as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoChecks;

impl ActChecker for NoChecks {
    fn check(&self, _act: &UnderstoodAct) -> Result<(), DomainRejection> {
        Ok(())
    }

    fn is_active(&self) -> bool {
        false
    }
}

/// The argument name a rejection's JSON Pointer names: `/value/0` names `value`.
pub(crate) fn argument_of(rejection: &DomainRejection) -> Option<String> {
    let pointer = rejection.argument.as_deref()?;
    let name = pointer.trim_start_matches('/').split('/').next()?;
    (!name.is_empty()).then(|| name.replace("~1", "/").replace("~0", "~"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_names_its_first_segment() {
        let rejection = DomainRejection::new("too_long", "k").on_argument("/value/0");
        assert_eq!(argument_of(&rejection).as_deref(), Some("value"));
        assert_eq!(argument_of(&DomainRejection::new("x", "k")), None);
    }
}
