//! The close gate's repo reads keep classified contention retryable as a typed
//! conflict, not as composed driver text.

use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::infra::posting::retry::AttemptError;

#[test]
fn repo_contention_retries_as_a_typed_conflict_and_other_failures_do_not() {
    assert!(
        matches!(
            AttemptError::from(RepoError::Conflict("database contention".into())),
            AttemptError::Conflict
        ),
        "a repo Conflict retries the close transaction"
    );
    for error in [
        RepoError::Db("storage failure".into()),
        RepoError::InvalidStoredMoney("corrupt segment".into()),
    ] {
        assert!(
            matches!(
                AttemptError::from(error),
                AttemptError::Business(DomainError::Internal(_))
            ),
            "a non-contention repo failure ends the close as an internal fault"
        );
    }
}
