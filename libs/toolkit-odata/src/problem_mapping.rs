//! Mapping from `OData` errors to canonical [`CanonicalError`].
//!
//! Handlers and the `OData` axum extractor return `CanonicalError` directly;
//! `IntoResponse for CanonicalError` (in `toolkit-canonical-errors`) renders the
//! wire `Problem` and stashes the original error into the response extensions
//! so the `canonical_error_middleware` (in `toolkit::api`) can log
//! `diagnostic()` and fill `instance` / `trace_id`. This gear is the single
//! source of truth for `Error → CanonicalError`; it does no logging of its own.

use toolkit_canonical_errors::CanonicalError;

use crate::Error;
use crate::errors::OdataError;

impl From<Error> for CanonicalError {
    fn from(err: Error) -> Self {
        use Error::{
            CursorInvalidBase64, CursorInvalidDirection, CursorInvalidFields, CursorInvalidJson,
            CursorInvalidKeys, CursorInvalidVersion, Db, FilterMismatch, InvalidCursor,
            InvalidFilter, InvalidLimit, InvalidOrderByField, OrderMismatch, OrderWithCursor,
            ParsingUnavailable,
        };

        match err {
            InvalidFilter(msg) => OdataError::invalid_argument()
                .with_field_violation(
                    "$filter",
                    format!("Invalid $filter: {msg}"),
                    "INVALID_FILTER",
                )
                .create(),

            InvalidOrderByField(field) => OdataError::invalid_argument()
                .with_field_violation(
                    "$orderby",
                    format!("Unsupported $orderby field: {field}"),
                    "INVALID_ORDERBY_FIELD",
                )
                .create(),

            InvalidCursor => OdataError::invalid_argument()
                .with_field_violation("cursor", "invalid cursor", "INVALID_CURSOR")
                .create(),

            CursorInvalidBase64 => OdataError::invalid_argument()
                .with_field_violation(
                    "cursor",
                    "invalid cursor: invalid base64url encoding",
                    "INVALID_CURSOR",
                )
                .create(),

            CursorInvalidJson => OdataError::invalid_argument()
                .with_field_violation("cursor", "invalid cursor: malformed JSON", "INVALID_CURSOR")
                .create(),

            CursorInvalidVersion => OdataError::invalid_argument()
                .with_field_violation(
                    "cursor",
                    "invalid cursor: unsupported version",
                    "INVALID_CURSOR",
                )
                .create(),

            CursorInvalidKeys => OdataError::invalid_argument()
                .with_field_violation(
                    "cursor",
                    "invalid cursor: empty or invalid keys",
                    "INVALID_CURSOR",
                )
                .create(),

            CursorInvalidFields => OdataError::invalid_argument()
                .with_field_violation(
                    "cursor",
                    "invalid cursor: empty or invalid fields",
                    "INVALID_CURSOR",
                )
                .create(),

            CursorInvalidDirection => OdataError::invalid_argument()
                .with_field_violation(
                    "cursor",
                    "invalid cursor: invalid sort direction",
                    "INVALID_CURSOR",
                )
                .create(),

            OrderMismatch => OdataError::invalid_argument()
                .with_field_violation(
                    "cursor",
                    "Order mismatch between cursor and query",
                    "ORDER_MISMATCH",
                )
                .create(),

            FilterMismatch => OdataError::invalid_argument()
                .with_field_violation(
                    "cursor",
                    "Filter mismatch between cursor and query",
                    "FILTER_MISMATCH",
                )
                .create(),

            InvalidLimit => OdataError::invalid_argument()
                .with_field_violation("$top", "Invalid limit parameter", "INVALID_LIMIT")
                .create(),

            // Surface both halves of the conflict so a client filtering by
            // `field` to render UI hints sees `$orderby` and `cursor`
            // simultaneously. Both entries carry the same reason code so
            // dispatch by `reason` still groups them.
            OrderWithCursor => OdataError::invalid_argument()
                .with_field_violation(
                    "$orderby",
                    "Cannot specify $orderby when cursor is present",
                    "ORDER_WITH_CURSOR",
                )
                .with_field_violation(
                    "cursor",
                    "Cannot specify cursor when $orderby is present",
                    "ORDER_WITH_CURSOR",
                )
                .create(),

            // For Internal-category errors, the caller-supplied detail flows
            // into `ctx.description` (recoverable via `CanonicalError::diagnostic()`)
            // and is logged by `canonical_error_middleware` once the response
            // reaches it. The wire `detail` stays opaque per DESIGN.md §3.6.
            Db(msg) => CanonicalError::internal(format!("OData Db error: {msg}")).create(),

            ParsingUnavailable(msg) => {
                CanonicalError::internal(format!("OData parsing unavailable: {msg}")).create()
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use toolkit_canonical_errors::Problem;
    use toolkit_gts::gts_id;

    const ODATA_RESOURCE_TYPE: &str = gts_id!("cf.core.odata.query.v1~");

    fn wire(err: Error) -> Problem {
        Problem::from(CanonicalError::from(err))
    }

    fn field_violations(p: &Problem) -> &Vec<serde_json::Value> {
        p.context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("invalid_argument context must carry field_violations[]")
    }

    fn assert_violation(v: &serde_json::Value, field: &str, reason: &str) {
        assert_eq!(
            v.get("field").and_then(|x| x.as_str()),
            Some(field),
            "unexpected violation field in {v}"
        );
        assert_eq!(
            v.get("reason").and_then(|x| x.as_str()),
            Some(reason),
            "unexpected violation reason in {v}"
        );
    }

    fn resource_type(p: &Problem) -> &str {
        p.context
            .get("resource_type")
            .and_then(|v| v.as_str())
            .expect("InvalidArgument from the OData scope must tag resource_type")
    }

    #[test]
    fn invalid_filter_populates_field_violation() {
        let p = wire(Error::InvalidFilter("malformed".into()));
        assert_eq!(p.status, 400);
        assert!(p.problem_type.contains("invalid_argument"));
        assert_eq!(resource_type(&p), ODATA_RESOURCE_TYPE);
        let violations = field_violations(&p);
        assert_eq!(violations.len(), 1);
        assert_violation(&violations[0], "$filter", "INVALID_FILTER");
        let desc = violations[0]
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or_default();
        assert!(desc.contains("malformed"), "description was {desc:?}");
    }

    #[test]
    fn invalid_orderby_field_populates_field_violation() {
        let p = wire(Error::InvalidOrderByField("unknown".into()));
        assert_eq!(p.status, 400);
        assert_eq!(resource_type(&p), ODATA_RESOURCE_TYPE);
        let violations = field_violations(&p);
        assert_eq!(violations.len(), 1);
        assert_violation(&violations[0], "$orderby", "INVALID_ORDERBY_FIELD");
    }

    #[test]
    fn cursor_invalid_base64_populates_field_violation() {
        let p = wire(Error::CursorInvalidBase64);
        assert_eq!(p.status, 400);
        assert_eq!(resource_type(&p), ODATA_RESOURCE_TYPE);
        let violations = field_violations(&p);
        assert_eq!(violations.len(), 1);
        assert_violation(&violations[0], "cursor", "INVALID_CURSOR");
    }

    #[test]
    fn order_with_cursor_emits_two_violations() {
        let p = wire(Error::OrderWithCursor);
        assert_eq!(p.status, 400);
        assert_eq!(resource_type(&p), ODATA_RESOURCE_TYPE);
        let violations = field_violations(&p);
        assert_eq!(
            violations.len(),
            2,
            "OrderWithCursor must surface both `$orderby` and `cursor`"
        );
        let fields: Vec<&str> = violations
            .iter()
            .filter_map(|v| v.get("field").and_then(|x| x.as_str()))
            .collect();
        assert!(fields.contains(&"$orderby"));
        assert!(fields.contains(&"cursor"));
        for v in violations {
            assert_eq!(
                v.get("reason").and_then(|x| x.as_str()),
                Some("ORDER_WITH_CURSOR")
            );
        }
    }

    #[test]
    fn order_mismatch_keys_to_cursor() {
        let p = wire(Error::OrderMismatch);
        let violations = field_violations(&p);
        assert_eq!(violations.len(), 1);
        assert_violation(&violations[0], "cursor", "ORDER_MISMATCH");
    }

    #[test]
    fn filter_mismatch_keys_to_cursor() {
        let p = wire(Error::FilterMismatch);
        let violations = field_violations(&p);
        assert_eq!(violations.len(), 1);
        assert_violation(&violations[0], "cursor", "FILTER_MISMATCH");
    }

    #[test]
    fn invalid_limit_keys_to_top() {
        let p = wire(Error::InvalidLimit);
        let violations = field_violations(&p);
        assert_eq!(violations.len(), 1);
        assert_violation(&violations[0], "$top", "INVALID_LIMIT");
    }

    #[test]
    fn db_error_maps_to_internal_with_diagnostic() {
        let canonical =
            CanonicalError::from(Error::Db("connection refused: 127.0.0.1:5432".into()));
        // Wire side: opaque internal envelope.
        let p = Problem::from(canonical.clone());
        assert_eq!(p.status, 500);
        assert!(p.problem_type.contains("internal"));
        // The wire `detail` is the canonical fixed string — never the raw msg.
        assert!(!p.detail.contains("127.0.0.1"));
        // Diagnostic side: descriptive cause preserved for `canonical_error_middleware`.
        let diag = canonical.diagnostic().expect("Internal carries diagnostic");
        assert!(
            diag.contains("connection refused"),
            "diagnostic was {diag:?}"
        );
    }

    #[test]
    fn parsing_unavailable_maps_to_internal_with_diagnostic() {
        let canonical = CanonicalError::from(Error::ParsingUnavailable("feature off"));
        assert_eq!(canonical.status_code(), 500);
        let diag = canonical.diagnostic().expect("Internal carries diagnostic");
        assert!(diag.contains("feature off"), "diagnostic was {diag:?}");
    }
}
