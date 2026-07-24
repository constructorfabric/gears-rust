//! Unit tests for `LicenseResolverError` and its canonical mapping.

use toolkit_canonical_errors::{CanonicalError, Problem};

use super::{FieldViolation, LicenseResolverError};

#[test]
fn invalid_request_carries_canonical_field_violations() {
    let err = LicenseResolverError::InvalidRequest {
        violations: vec![
            FieldViolation::new(
                format!(
                    "{}/metadata/model_name",
                    toolkit_gts::gts_id!("cf.core.lic.res.v1~cf.genai.llm_gateway.model_usage.v1~")
                ),
                "model_name must be a string",
                "SCHEMA_MISMATCH",
            ),
            FieldViolation::new(
                "subject/type",
                "contract type is required",
                "MISSING_DOMAIN_TYPE",
            ),
        ],
    };

    // Display reports the violation count without leaking field contents.
    assert_eq!(err.to_string(), "invalid request: 2 violation(s)");

    // The canonical FieldViolation is serde-ready (maps onto InvalidArgument).
    let LicenseResolverError::InvalidRequest { violations } = &err else {
        panic!("expected InvalidRequest");
    };
    let json = serde_json::to_value(&violations[0]).unwrap();
    assert_eq!(
        json.get("reason").and_then(|v| v.as_str()),
        Some("SCHEMA_MISMATCH")
    );
    assert!(json.get("field").is_some());
}

#[test]
fn service_unavailable_reports_reason() {
    let err = LicenseResolverError::ServiceUnavailable("backend timeout".to_owned());
    assert_eq!(err.to_string(), "service unavailable: backend timeout");
}

#[test]
fn unauthorized_maps_to_permission_denied() {
    let canonical: CanonicalError = LicenseResolverError::Unauthorized.into();
    assert_eq!(canonical.status_code(), 403);
    assert!(
        canonical.gts_type().contains("permission_denied"),
        "unexpected gts type: {}",
        canonical.gts_type()
    );
}

#[test]
fn invalid_request_maps_to_invalid_argument_400() {
    let err = LicenseResolverError::InvalidRequest {
        violations: vec![FieldViolation::new(
            "subject/type",
            "contract type is required",
            "MISSING_DOMAIN_TYPE",
        )],
    };
    let canonical: CanonicalError = err.into();
    assert_eq!(canonical.status_code(), 400);
    assert!(canonical.gts_type().contains("invalid_argument"));

    // The full path to an RFC-9457 body works for consumers.
    let problem = Problem::from_error(&canonical).expect("problem renders");
    assert_eq!(problem.status, 400);
}

#[test]
fn empty_invalid_request_still_maps_to_400() {
    let canonical: CanonicalError =
        LicenseResolverError::InvalidRequest { violations: vec![] }.into();
    assert_eq!(canonical.status_code(), 400);
}

#[test]
fn service_unavailable_maps_to_503() {
    let diagnostic = "connection failed: postgres://secret@license-db.internal";
    let canonical: CanonicalError =
        LicenseResolverError::ServiceUnavailable(diagnostic.to_owned()).into();
    assert_eq!(canonical.status_code(), 503);

    let problem = Problem::from_error(&canonical).expect("problem renders");
    assert_eq!(problem.detail, "License service temporarily unavailable");
    assert!(
        !problem.detail.contains(diagnostic),
        "internal diagnostic must not be exposed in the public problem detail"
    );
}

#[test]
fn no_plugin_and_internal_map_to_500() {
    let no_plugin: CanonicalError = LicenseResolverError::NoPluginAvailable.into();
    assert_eq!(no_plugin.status_code(), 500);

    let internal: CanonicalError = LicenseResolverError::Internal("boom".to_owned()).into();
    assert_eq!(internal.status_code(), 500);
}
