//! Ergonomic result types for API handlers
//!
//! This module provides type aliases and conversions to make error handling
//! in HTTP handlers more concise and uniform.

use crate::api::problem::Problem;

/// Standard result type for API operations
///
/// Use this throughout your handlers for consistent error handling:
///
/// ```ignore
/// async fn handler() -> ApiResult<Json<User>> {
///     let user = fetch_user().await?;  // auto-converts errors to Problem
///     Ok(Json(user))
/// }
/// ```
///
/// The `?` operator automatically converts any error implementing
/// `Into<Problem>` (including `modkit_odata::Error`) into a Problem.
/// Problem implements `IntoResponse`, so Axum will automatically convert it
/// to an HTTP response when returned from a handler.
pub type ApiResult<T = ()> = Result<T, Problem>;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "result_tests.rs"]
mod tests;
