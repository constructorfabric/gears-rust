//! Re-exports and convenience constructors for Problem types

use http::StatusCode;

pub use modkit_errors::problem::{
    APPLICATION_PROBLEM_JSON, Problem, ValidationError, ValidationErrorResponse,
    ValidationViolation,
};

// Optional convenience constructors that return `Problem` directly
pub fn bad_request(detail: impl Into<String>) -> Problem {
    Problem::new(StatusCode::BAD_REQUEST, "Bad Request", detail)
}

pub fn not_found(detail: impl Into<String>) -> Problem {
    Problem::new(StatusCode::NOT_FOUND, "Not Found", detail)
}

pub fn conflict(detail: impl Into<String>) -> Problem {
    Problem::new(StatusCode::CONFLICT, "Conflict", detail)
}

pub fn internal_error(detail: impl Into<String>) -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Internal Server Error",
        detail,
    )
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "problem_tests.rs"]
mod tests;
