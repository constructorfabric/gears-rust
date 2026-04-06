use super::*;

#[test]
fn test_domain_error_to_problem_not_found() {
    let err = DomainError::not_found("gts.x.core.events.test.v1~");
    let problem: Problem = err.into();
    assert_eq!(problem.status, StatusCode::NOT_FOUND);
}

#[test]
fn test_domain_error_to_problem_already_exists() {
    let err = DomainError::already_exists("gts.x.core.events.test.v1~");
    let problem: Problem = err.into();
    assert_eq!(problem.status, StatusCode::CONFLICT);
}

#[test]
fn test_domain_error_to_problem_invalid_gts_id() {
    let err = DomainError::invalid_gts_id("bad format");
    let problem: Problem = err.into();
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
}

#[test]
fn test_domain_error_to_problem_validation_failed() {
    let err = DomainError::validation_failed("schema invalid");
    let problem: Problem = err.into();
    assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn test_domain_error_to_problem_not_in_ready_mode() {
    let err = DomainError::NotInReadyMode;
    let problem: Problem = err.into();
    assert_eq!(problem.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn test_domain_error_to_problem_ready_commit_failed() {
    use crate::domain::error::ValidationError;
    let err = DomainError::ReadyCommitFailed(vec![
        ValidationError::new("gts.test1~", "error1"),
        ValidationError::new("gts.test2~", "error2"),
        ValidationError::new("gts.test3~", "error3"),
    ]);
    let problem: Problem = err.into();
    assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn test_domain_error_to_problem_internal() {
    let err = DomainError::Internal(anyhow::anyhow!("test error"));
    let problem: Problem = err.into();
    assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
}
