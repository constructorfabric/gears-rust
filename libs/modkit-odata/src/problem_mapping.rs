//! Mapping from `OData` errors to Problem (pure data)
//!
//! This provides a baseline conversion from `OData` errors to RFC 9457 Problem
//! without HTTP framework dependencies. The HTTP layer in `modkit` adds
//! instance paths and trace IDs before the Problem is converted to an HTTP response.

use crate::Error;
use crate::errors::ErrorCode;
use modkit_errors::problem::Problem;

impl From<Error> for Problem {
    fn from(err: Error) -> Self {
        use Error::{
            CursorInvalidBase64, CursorInvalidDirection, CursorInvalidFields, CursorInvalidJson,
            CursorInvalidKeys, CursorInvalidVersion, Db, FilterMismatch, InvalidCursor,
            InvalidFilter, InvalidLimit, InvalidOrderByField, OrderMismatch, OrderWithCursor,
            ParsingUnavailable,
        };

        match err {
            // Filter parsing errors → 422
            InvalidFilter(msg) => ErrorCode::odata_errors_invalid_filter_v1()
                .as_problem(format!("Invalid $filter: {msg}")),

            // OrderBy parsing and validation errors → 422
            InvalidOrderByField(field) => ErrorCode::odata_errors_invalid_orderby_v1()
                .as_problem(format!("Unsupported $orderby field: {field}")),

            // All cursor-related errors → 422
            InvalidCursor
            | CursorInvalidBase64
            | CursorInvalidJson
            | CursorInvalidVersion
            | CursorInvalidKeys
            | CursorInvalidFields
            | CursorInvalidDirection => {
                ErrorCode::odata_errors_invalid_cursor_v1().as_problem(format!("{err}"))
            }

            // Pagination validation errors → 422
            OrderMismatch => ErrorCode::odata_errors_invalid_orderby_v1()
                .as_problem("Order mismatch between cursor and query"),

            FilterMismatch => ErrorCode::odata_errors_invalid_filter_v1()
                .as_problem("Filter mismatch between cursor and query"),

            InvalidLimit => {
                ErrorCode::odata_errors_invalid_filter_v1().as_problem("Invalid limit parameter")
            }

            OrderWithCursor => ErrorCode::odata_errors_invalid_cursor_v1()
                .as_problem("Cannot specify both $orderby and cursor parameters"),

            // Database errors → 500 (should be caught earlier)
            Db(_msg) => {
                // Use filter error as safe default for unexpected DB errors
                ErrorCode::odata_errors_internal_v1()
                    .as_problem("An internal error occurred while processing the OData query")
            }

            // Configuration errors → 500 (feature not enabled)
            ParsingUnavailable(msg) => ErrorCode::odata_errors_internal_v1()
                .as_problem(format!("OData parsing unavailable: {msg}")),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "problem_mapping_tests.rs"]
mod tests;
