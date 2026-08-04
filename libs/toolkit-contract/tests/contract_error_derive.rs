//! Integration tests for `#[derive(ContractError)]` covering:
//! - Server-side: `From<MyError> for Problem` populates `error_code`,
//!   `error_domain`, GTS URI from category, HTTP status, and
//!   `context["data"]`.
//! - Client-side: `TryFrom<Problem> for MyError` reconstructs the typed
//!   variant; unknown codes round-trip back as the original `Problem`.
//! - Round-trip across JSON serialization.

use serde::{Deserialize, Serialize};
use toolkit_contract::{ContractError, Problem};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ContractError)]
#[error_domain("billing.v1")]
#[non_exhaustive]
pub enum BillingError {
    #[error_code("INSUFFICIENT_FUNDS")]
    #[canonical(FailedPrecondition)]
    InsufficientFunds { available: u64, required: u64 },

    #[error_code("ACCOUNT_FROZEN")]
    #[canonical(FailedPrecondition)]
    AccountFrozen { reason: String },

    #[error_code("RATE_LIMIT")]
    #[canonical(ResourceExhausted)]
    RateLimit { retry_after_sec: u32 },

    #[error_code("MAINTENANCE")]
    #[canonical(ServiceUnavailable)]
    Maintenance,
}

#[test]
fn to_problem_sets_extension_fields_and_category() {
    let err = BillingError::InsufficientFunds {
        available: 100,
        required: 500,
    };
    let problem: Problem = err.into();

    assert_eq!(problem.error_code.as_deref(), Some("INSUFFICIENT_FUNDS"));
    assert_eq!(problem.error_domain.as_deref(), Some("billing.v1"));
    assert!(
        problem.problem_type.contains("failed_precondition"),
        "got {}",
        problem.problem_type
    );
    assert_eq!(problem.status, 400);
    assert_eq!(problem.title, "Failed precondition");
}

#[test]
fn to_problem_named_fields_land_in_context_data() {
    let err = BillingError::InsufficientFunds {
        available: 100,
        required: 500,
    };
    let problem: Problem = err.into();
    let data = &problem.context["data"];
    assert_eq!(data["available"].as_u64(), Some(100));
    assert_eq!(data["required"].as_u64(), Some(500));
}

#[test]
fn to_problem_unit_variant_has_empty_data() {
    let err = BillingError::Maintenance;
    let problem: Problem = err.into();
    assert_eq!(problem.error_code.as_deref(), Some("MAINTENANCE"));
    assert_eq!(problem.status, 503);
    assert!(problem.context["data"].is_object());
    assert_eq!(
        problem.context["data"].as_object().expect("object").len(),
        0
    );
}

#[test]
fn try_from_problem_round_trips_named_variant() {
    let original = BillingError::InsufficientFunds {
        available: 42,
        required: 100,
    };
    let problem: Problem = original.clone().into();
    let recovered = BillingError::try_from(problem).expect("known code round-trips");
    assert_eq!(recovered, original);
}

#[test]
fn try_from_problem_round_trips_unit_variant() {
    let problem: Problem = BillingError::Maintenance.into();
    let recovered = BillingError::try_from(problem).expect("unit variant round-trips");
    assert_eq!(recovered, BillingError::Maintenance);
}

#[test]
fn try_from_problem_returns_envelope_for_unknown_code() {
    // PRD §FR-unknown-code: unknown (error_domain, error_code) pairs must
    // not crash the client — they bounce back as the original Problem so
    // the caller can fall through to generic error handling.
    let mut problem = Problem {
        problem_type: "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~".into(),
        title: "Internal".into(),
        status: 500,
        detail: "synthetic".into(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({}),
        error_code: Some("UNHEARD_OF_ERROR".into()),
        error_domain: Some("billing.v1".into()),
    };
    let err = BillingError::try_from(problem.clone()).unwrap_err();
    // The Problem is returned unmodified — diagnostic surface preserved.
    assert_eq!(err.error_code, problem.error_code);
    // Sanity: it's the same object, not silently re-serialized.
    problem.detail.push_str("");
    assert_eq!(err.detail, "synthetic");
}

#[test]
fn try_from_problem_returns_envelope_when_data_field_missing() {
    // A peer (or stale client) that sent the right code+domain but a
    // malformed payload must NOT succeed in producing a half-populated
    // typed variant. Return the original Problem to surface the issue.
    let problem = Problem {
        problem_type: "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~".into(),
        title: "Failed precondition".into(),
        status: 400,
        detail: "missing data payload".into(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({ "data": { "available": 100 } }), // `required` missing
        error_code: Some("INSUFFICIENT_FUNDS".into()),
        error_domain: Some("billing.v1".into()),
    };
    let err = BillingError::try_from(problem).unwrap_err();
    assert_eq!(err.error_code.as_deref(), Some("INSUFFICIENT_FUNDS"));
}

#[test]
fn round_trip_survives_json_serialization() {
    // The whole point: typed enum → Problem → JSON wire → Problem → typed
    // enum, all without loss. This is the test that proves PRD wire-compat
    // claims are real.
    let original = BillingError::RateLimit {
        retry_after_sec: 30,
    };
    let problem: Problem = original.clone().into();

    let json = serde_json::to_string(&problem).expect("serialize");
    let parsed: Problem = serde_json::from_str(&json).expect("deserialize");

    // Make sure the JSON itself carries the two PRD extension fields at
    // top level — that's the on-wire surface another team's parser will
    // be looking at.
    let raw: serde_json::Value = serde_json::from_str(&json).expect("raw parse");
    assert_eq!(raw["error_code"], "RATE_LIMIT");
    assert_eq!(raw["error_domain"], "billing.v1");
    assert_eq!(raw["data"], serde_json::Value::Null); // not hoisted to top-level
    assert_eq!(raw["context"]["data"]["retry_after_sec"], 30);

    let recovered = BillingError::try_from(parsed).expect("round trip");
    assert_eq!(recovered, original);
}

// --- Forward/interop tolerance: absent optional vs required fields (M-5) -----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ContractError)]
#[error_domain("orders.v1")]
#[non_exhaustive]
pub enum OrderRejection {
    #[error_code("REJECTED")]
    #[canonical(FailedPrecondition)]
    Rejected {
        reason: String,
        hint: Option<String>,
    },
}

#[test]
fn try_from_tolerates_absent_optional_field() {
    // A peer (or older/newer variant) that OMITS an optional field entirely must
    // still reconstruct the typed variant with `None` — not bounce to the
    // generic envelope (M-5). Absent key is treated as JSON null.
    let mut problem: Problem = OrderRejection::Rejected {
        reason: "declined".into(),
        hint: Some("retry later".into()),
    }
    .into();
    problem
        .context
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
        .expect("data object")
        .remove("hint");
    let back = OrderRejection::try_from(problem).expect("typed reconstruction");
    assert_eq!(
        back,
        OrderRejection::Rejected {
            reason: "declined".into(),
            hint: None,
        }
    );
}

#[test]
fn try_from_bounces_when_required_field_absent() {
    // A missing REQUIRED field must still fail and bounce the original Problem —
    // strictness is preserved for non-optional data.
    let mut problem: Problem = OrderRejection::Rejected {
        reason: "declined".into(),
        hint: None,
    }
    .into();
    problem
        .context
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
        .expect("data object")
        .remove("reason");
    assert!(OrderRejection::try_from(problem).is_err());
}

// --- Client-side total reconstruction via the fallback bridge ---------------
// `#[contract_error(fallback)]` generates `From<TransportError> for MyError`
// (gated on `rest-client`): a `Problem` is offered to `TryFrom` first, and any
// un-reconstructable transport/protocol error lands in the fallback variant.
#[cfg(feature = "rest-client")]
mod transport_fallback {
    use super::*;
    use toolkit_contract::runtime::transport_error::TransportError;

    // The `#[contract_error(fallback)]` variant holds a full `Problem` by value
    // (the derive constructs it unboxed), which trips `large_enum_variant` under
    // `-D clippy::perf`. That's inherent to the fallback pattern being tested;
    // enum size is irrelevant in a test, so allow it here.
    #[derive(Debug, Clone, Serialize, Deserialize, ContractError)]
    #[error_domain("orders.v1")]
    #[non_exhaustive]
    #[allow(clippy::large_enum_variant)]
    pub enum OrderError {
        #[error_code("NOT_FOUND")]
        #[canonical(NotFound)]
        NotFound { id: String },

        #[error_code("UNKNOWN")]
        #[canonical(Internal)]
        #[contract_error(fallback)]
        Unknown { problem: Problem },
    }

    #[test]
    fn reconstructs_typed_variant_from_problem() {
        let problem: Problem = OrderError::NotFound { id: "abc".into() }.into();
        let via_transport: OrderError = TransportError::problem(problem).into();
        match via_transport {
            OrderError::NotFound { id } => assert_eq!(id, "abc"),
            other => panic!("expected typed NotFound, got {other:?}"),
        }
    }

    #[test]
    fn unknown_problem_routes_to_fallback() {
        // A foreign Problem (billing.v1 / MAINTENANCE) unknown to OrderError.
        let foreign: Problem = BillingError::Maintenance.into();
        let err: OrderError = TransportError::problem(foreign).into();
        assert!(matches!(err, OrderError::Unknown { .. }));
    }

    #[test]
    fn non_problem_transport_error_routes_to_fallback() {
        let err: OrderError = TransportError::network("dns fail").into();
        match err {
            OrderError::Unknown { problem } => assert!(problem.status >= 500),
            other => panic!("expected fallback Unknown, got {other:?}"),
        }
    }
}
