//! Centralized `OData` error mapping
//!
//! This module adds HTTP-specific context (instance path, trace ID) to `OData` errors.
//! The core Error → Problem mapping is owned by modkit-odata.

use crate::api::problem::Problem;
use modkit_odata::Error as ODataError;

/// Extract trace ID from current tracing span
#[inline]
fn current_trace_id() -> Option<String> {
    tracing::Span::current()
        .id()
        .map(|id| id.into_u64().to_string())
}

/// Returns a fully contextualized Problem for `OData` errors.
///
/// This function maps all `modkit_odata::Error` variants to appropriate system
/// error codes from the framework catalog. The `instance` parameter should
/// be the request path.
///
/// # Arguments
/// * `err` - The `OData` error to convert
/// * `instance` - The request path (e.g., "/api/user-management/v1/users")
/// * `trace_id` - Optional trace ID (uses current span if None)
pub fn odata_error_to_problem(
    err: &ODataError,
    instance: &str,
    trace_id: Option<String>,
) -> Problem {
    use modkit_odata::Error as OE;

    // Add logging for errors that need it before conversion
    match err {
        OE::Db(msg) => {
            tracing::error!(error = %msg, "Unexpected database error in OData layer");
        }
        OE::ParsingUnavailable(msg) => {
            tracing::error!(error = %msg, "OData parsing unavailable");
        }
        _ => {}
    }

    // Delegate to modkit-odata's base mapping (single source of truth)
    let mut problem: Problem = err.clone().into();

    // Add HTTP-specific context
    problem = problem.with_instance(instance);

    let trace_id = trace_id.or_else(current_trace_id);
    if let Some(tid) = trace_id {
        problem = problem.with_trace_id(tid);
    }

    problem
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod tests;
