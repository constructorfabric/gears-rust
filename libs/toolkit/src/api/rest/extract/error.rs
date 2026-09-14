//! Shared canonical-error plumbing for `toolkit::api::rest::extract`.

use axum::extract::rejection::JsonRejection;
use toolkit_canonical_errors::{CanonicalError, Http, resource_error};

/// Resource-error marker for canonical errors originating from generic
/// toolkit/framework code with no single domain resource in scope - not
/// specific to JSON, reusable by future extractor-level fixes
/// (`extract::Query`, `extract::Path`). Mirrors
/// `toolkit_odata::errors::OdataError`. `error` is a private module (see
/// `extract/mod.rs`), so this `pub` item is still crate-internal in
/// effect - nothing outside `toolkit` constructs it directly.
#[resource_error(gts_id!("cf.core.http.request.v1~"))]
pub struct GenericResourceError;

/// Machine-readable reason code per `JsonRejection` kind, matching the
/// vocabulary already shipped in `gears/bss/ledger`'s own `CanonicalJson`.
///
/// `JsonRejection` is `#[non_exhaustive]`, so this match needs some arm for
/// a variant it doesn't know about, and each of the four known variants
/// maps to a genuinely different code - there's no existing "unclassified"
/// bucket to fall back to honestly. In debug/test builds the wildcard
/// arm panics (via `debug_assert!`, matching
/// `toolkit-canonical-errors`' own same-status-class check), forcing this
/// match to be updated the moment a future axum upgrade actually
/// introduces a new variant. In release builds it does not panic - most
/// routers, including this crate's own test apps, have no guaranteed panic
/// isolation, so a semver-minor axum bump must not be able to abort a
/// request-serving task in production. Instead it logs the unhandled
/// variant and degrades to `"unclassified_json_rejection"` - an honest "we
/// don't recognize this" code, not a guess dressed up as one of the four
/// known ones.
pub fn json_rejection_code(rejection: &JsonRejection) -> &'static str {
    match rejection {
        JsonRejection::JsonSyntaxError(_) => "json_syntax_error",
        JsonRejection::JsonDataError(_) => "invalid_json_body",
        JsonRejection::MissingJsonContentType(_) => "missing_json_content_type",
        JsonRejection::BytesRejection(_) => "json_body_read_error",
        _ => {
            debug_assert!(
                false,
                "unhandled JsonRejection variant, update json_rejection_code: {rejection}"
            );
            tracing::error!(
                rejection = %rejection,
                "extract::Json: unhandled JsonRejection variant, update json_rejection_code"
            );
            "unclassified_json_rejection"
        }
    }
}

/// Maps a `JsonRejection` to a `CanonicalError`. Not a `From` impl - both
/// `JsonRejection` and `CanonicalError` are foreign to this crate, so the
/// orphan rule forbids it; `extract::Json::from_request` calls this
/// explicitly instead of relying on `?`'s automatic conversion.
pub fn json_rejection_to_canonical(rejection: &JsonRejection) -> CanonicalError {
    let code = json_rejection_code(rejection);
    // `.status()` / `.body_text()` are inherent on every axum rejection
    // type (incl. nested composites, e.g. `BytesRejection`'s own
    // `LengthLimitError`/`UnknownBodyError`) - resolves to axum's own
    // intended status per failure kind with no variant matching needed
    // for the status itself.
    rejection_to_canonical(
        "body",
        code,
        rejection.status().as_u16(),
        rejection.body_text(),
    )
}

/// Hard cap on how many characters of a rejection's raw message ever reach
/// the wire in `field_violations[].description`. Every message this module
/// family actually produces today is well under 200 characters (see the
/// extractor test suites) - well-formed client traffic never comes close to
/// this cap. It exists only to stop a client from inflating the response by
/// submitting a pathologically long value that axum/serde echo back
/// verbatim (e.g. an extremely long unknown JSON field name under
/// `#[serde(deny_unknown_fields)]`) - the description text isn't sensitive
/// (see `rejection_to_canonical`'s doc comment), just potentially large.
const MAX_FIELD_VIOLATION_DESCRIPTION_CHARS: usize = 500;

/// Shared shape behind every `*_rejection_to_canonical` in this module
/// family (`extract::json`, `extract::path`, `extract::query`). Dispatches
/// on `status`'s class first, mirroring
/// `canonical_error_layer::wrap_foreign_response`'s own 4xx/5xx split: a
/// `>= 500` status is unambiguously this platform's own fault, not the
/// client's - a rejection is never 3xx (checked against every
/// `#[status = ...]` in axum-core/axum's own source: extraction failure has
/// no redirect concept), so `>= 500` and 4xx-or-below are the only two
/// classes possible. The `>= 500` branch maps to a real `internal`
/// `CanonicalError` regardless of which extractor discovered it - today
/// only `extract::Path`'s `WrongNumberOfParameters`/`UnsupportedType`/
/// `MissingPathParams` ever produce one, but the check applies to all
/// three extractors uniformly, so a hypothetical future axum rejection
/// with a 5xx status (from any of the three extractors) can't slip through
/// `GenericResourceError::invalid_argument()` and violate
/// `.with_override`'s own same-status-class invariant (a `4xx` category
/// cannot carry a `5xx` override). Otherwise builds
/// `GenericResourceError::invalid_argument()` with a single field violation
/// and a per-occurrence status override. Each extractor keeps only its own
/// variant-to-`(field, code, status)` classification local; this is the one
/// place the actual `CanonicalError` construction happens.
///
/// `message` (axum/serde's own rejection text) is put on the wire as-is in
/// the `invalid_argument` case, not redacted: it only ever describes the
/// caller's own submitted input or the request schema's own field/enum
/// names, both already necessarily public (a client needs the schema to
/// build a valid request). It's length-capped for a different
/// reason: bandwidth, not confidentiality. In the `internal` case `message`
/// becomes the private diagnostic (`#[serde(skip)]`, logged server-side
/// only) - never wire-visible regardless of length.
pub fn rejection_to_canonical(
    field: &str,
    code: &str,
    status: u16,
    message: String,
) -> CanonicalError {
    if status >= 500 {
        return if status == 500 {
            CanonicalError::internal(message).create()
        } else {
            CanonicalError::internal(message)
                .with_override(Http::status_code(status))
                .create()
        };
    }
    GenericResourceError::invalid_argument()
        .with_field_violation(field, truncate_description(message), code)
        .with_override(Http::status_code(status))
        .create()
}

const TRUNCATION_SUFFIX: &str = "... (truncated)";

fn truncate_description(message: String) -> String {
    if message.chars().count() <= MAX_FIELD_VIOLATION_DESCRIPTION_CHARS {
        return message;
    }
    tracing::debug!(
        original_len = message.len(),
        "extractor rejection message exceeded MAX_FIELD_VIOLATION_DESCRIPTION_CHARS, truncating before sending to client"
    );
    let mut truncated: String = message
        .chars()
        .take(MAX_FIELD_VIOLATION_DESCRIPTION_CHARS - TRUNCATION_SUFFIX.chars().count())
        .collect();
    truncated.push_str(TRUNCATION_SUFFIX);
    truncated
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn short_message_passes_through_unchanged() {
        assert_eq!(
            truncate_description("invalid digit found in string".to_owned()),
            "invalid digit found in string"
        );
    }

    #[test]
    fn rejection_to_canonical_maps_any_5xx_status_to_internal_not_invalid_argument() {
        // A hypothetical future rejection (from any of the three
        // extractors) with a 5xx status must never reach
        // `GenericResourceError::invalid_argument().with_override(...)` -
        // that would violate the same-status-class invariant
        // (`invalid_argument`'s default class is 4xx). Passing a `field`/
        // `code` that would only make sense for the 4xx branch proves the
        // `>= 500` check really does short-circuit before either is used.
        let err = rejection_to_canonical("body", "some_4xx_only_code", 503, "boom".to_owned());
        let problem: toolkit_canonical_errors::Problem = err.into();
        let json = serde_json::to_value(&problem).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
                "title": "Internal",
                "status": 503,
                "detail": "An internal error occurred. Please retry later.",
                "context": {},
            })
        );
    }

    #[test]
    fn oversized_message_is_truncated_with_a_marker() {
        let huge = "x".repeat(MAX_FIELD_VIOLATION_DESCRIPTION_CHARS + 1000);
        let result = truncate_description(huge);
        assert_eq!(
            result.chars().count(),
            MAX_FIELD_VIOLATION_DESCRIPTION_CHARS
        );
        assert_eq!(
            result,
            format!(
                "{}... (truncated)",
                "x".repeat(
                    MAX_FIELD_VIOLATION_DESCRIPTION_CHARS - TRUNCATION_SUFFIX.chars().count()
                )
            )
        );
    }
}
