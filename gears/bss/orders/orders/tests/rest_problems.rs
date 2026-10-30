//! D-219: the Orders REST error gear owns the Problem encoding and the reason → HTTP status
//! table, and decodes a stored Orders Problem back into the SDK error by
//! `(error_domain, error_code)` plus `context.data`.
//!
//! `fixtures/rest-problem-golden.json` holds the exact Problem bytes the pre-D-219 SDK encoder
//! (`OrdersError::into_problem`, `ReasonMapping::http_status`) produced at 53091e0d5 for every
//! registered reason, every settled diagnostic shape and every infrastructure error. These tests
//! prove the wire unchanged and every reason round-trips: boundary refusals and infrastructure
//! errors byte for byte; settled refusals as JSON, because their body is rendered from a
//! `serde_json::Value` whose key order depends on the build's `serde_json` features.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a corrupt frozen fixture must fail loudly; `allow`, not `expect`, because not every \
              test in the file unwraps or expects"
)]
use bss_orders::api::rest::{self, error::RefusalEnvelope, wire};
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::catalog::{OPERATIONS, Reason};
use bss_orders_sdk::errors::RefusalDiagnostics;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use toolkit_canonical_errors::{CanonicalError, Problem};

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/rest-problem-golden.json")).unwrap()
}
fn catalog_json() -> Value {
    serde_json::from_str(include_str!("../../orders-sdk/contracts/catalog.json")).unwrap()
}
fn reason_of(entry: &Value) -> Reason {
    serde_json::from_value(entry["reason"].clone()).unwrap()
}

/// Every boundary refusal and infrastructure error answers the same status and body bytes as
/// before D-219 (`Problem`'s `IntoResponse` serializes the struct; the status is `status`).
#[test]
fn boundary_and_infrastructure_problems_are_byte_identical_to_the_pre_d219_wire() {
    let golden = golden();
    let entries = golden["boundary_refusals"].as_array().unwrap();
    assert_eq!(entries.len(), Reason::ALL.len());
    let mut seen = BTreeSet::new();
    for entry in entries {
        let reason = reason_of(entry);
        assert!(seen.insert(reason.mapping().code));
        let problem = rest::problem(OrdersError::Refused(reason));
        assert_eq!(problem.status.map(u64::from), entry["status"].as_u64());
        assert_eq!(
            serde_json::to_string(&problem).unwrap(),
            entry["body"].as_str().unwrap(),
            "{reason:?}"
        );
    }
    for entry in golden["infrastructure"].as_array().unwrap() {
        let error = match entry["error"].as_str().unwrap() {
            "unavailable" => OrdersError::Unavailable,
            "integration" => OrdersError::Integration,
            "internal" => OrdersError::Internal,
            "outcome-unknown" => OrdersError::OutcomeUnknown,
            other => panic!("unknown golden error {other}"),
        };
        let problem = rest::problem(error);
        assert_eq!(problem.status.map(u64::from), entry["status"].as_u64());
        assert_eq!(
            serde_json::to_string(&problem).unwrap(),
            entry["body"].as_str().unwrap()
        );
    }
}

/// Every settled refusal snapshot (what the engine stores and REST replays byte for byte) is
/// unchanged, and decodes into the SDK error with the same reason and typed diagnostics, through
/// the contract-error decode and not the HTTP status. Re-encoding the decoded error yields the
/// same bytes again.
#[test]
fn every_settled_refusal_is_identical_json_and_round_trips_through_the_sdk_error() {
    let golden = golden();
    let mut reasons = BTreeSet::new();
    for entry in golden["settled_refusals"].as_array().unwrap() {
        let reason = reason_of(entry);
        reasons.insert(reason.mapping().code);
        let data = entry["data"].clone();
        let problem = rest::error::settled_refusal_problem(reason, data.clone());
        // The engine stores `json_value(&problem)` with `wire::reason_status` as the status.
        let stored = serde_json::to_value(&problem).unwrap();
        assert_eq!(
            u64::from(wire::reason_status(reason)),
            entry["status"].as_u64().unwrap()
        );
        assert_eq!(problem.status, Some(wire::reason_status(reason)));
        // The stored body is rendered from a `serde_json::Value`, whose key order follows the
        // build's serde_json features (`preserve_order` is on in workspace builds, off in a
        // single-crate one), so the body is compared as JSON, not as bytes.
        let golden_body: serde_json::Value =
            serde_json::from_str(entry["body"].as_str().unwrap()).unwrap();
        assert_eq!(stored, golden_body, "{reason:?}");
        // Decode: stored body → Problem → (error_domain, error_code) + context.data.
        let decoded: Problem = serde_json::from_value(stored).unwrap();
        let envelope = RefusalEnvelope::try_from(decoded).unwrap();
        assert_eq!(envelope.reason, reason);
        let expected: RefusalDiagnostics = serde_json::from_value(data.clone()).unwrap();
        assert_eq!(envelope.diagnostics, expected);
        assert_eq!(serde_json::to_value(&envelope.diagnostics).unwrap(), data);
        let error = OrdersError::from(envelope);
        assert_eq!(error.reason(), Some(reason));
        // Encode the SDK error again: the same body the engine stored.
        assert_eq!(
            serde_json::to_value(rest::problem(error)).unwrap(),
            golden_body,
            "{reason:?}"
        );
    }
    assert_eq!(reasons.len(), Reason::ALL.len(), "every reason is covered");
}

/// The decode never reads the status and bounces back anything that is not an Orders refusal:
/// another domain, an unregistered code, a mistyped diagnostic.
#[test]
fn the_contract_error_decode_keys_on_domain_and_code_and_bounces_the_rest() {
    let mut problem = rest::error::settled_refusal_problem(
        Reason::VersionConflict,
        json!({"current_version": 2, "draft_revision": 5}),
    );
    // A status the decode must ignore.
    problem.status = Some(599);
    let envelope = RefusalEnvelope::try_from(problem.clone()).unwrap();
    assert_eq!(envelope.reason, Reason::VersionConflict);
    assert_eq!(envelope.diagnostics.draft_revision.map(i64::from), Some(5));
    for mutate in [
        (|p: &mut Problem| p.error_domain = Some("cluster.v1".to_owned())) as fn(&mut Problem),
        |p| p.error_domain = None,
        |p| p.error_code = Some("NOT_A_REGISTERED_CODE".to_owned()),
        |p| p.context = json!({"data": {"current_version": 0}}),
        |p| p.context = json!({"data": {"state": "invented"}}),
    ] {
        let mut bad = problem.clone();
        mutate(&mut bad);
        let bounced = RefusalEnvelope::try_from(bad.clone()).unwrap_err();
        assert_eq!(
            serde_json::to_value(bounced).unwrap(),
            serde_json::to_value(bad).unwrap()
        );
    }
    // Absent `context.data` is no diagnostics.
    let mut bare = problem;
    bare.context = json!({});
    assert_eq!(
        RefusalEnvelope::try_from(bare).unwrap().diagnostics,
        RefusalDiagnostics::default()
    );
}

/// D-219 records why the canonical round trip cannot carry an Orders refusal: for most reasons
/// `TryFrom<Problem> for CanonicalError` rejects the contract-error context, and where it succeeds
/// it drops `error_code` and `context.data`. A canonical error still projects infallibly.
#[test]
fn the_canonical_round_trip_cannot_carry_an_orders_refusal() {
    let mut rejected = 0;
    for reason in Reason::ALL {
        let problem = rest::problem(OrdersError::Refused(*reason));
        match CanonicalError::try_from(problem) {
            Err(_) => rejected += 1,
            Ok(canonical) => {
                let back = Problem::from(canonical.clone());
                assert_eq!(back.error_code, None, "{reason:?}");
                // The projection is total; without a code it cannot name the reason.
                assert_eq!(OrdersError::from(canonical).reason(), None);
            }
        }
    }
    // Measured: 70 of the 90 reasons are rejected; the 20 not_found, already_exists and
    // service_unavailable reasons convert without their code and diagnostics.
    assert_eq!((rejected, Reason::ALL.len()), (70, 90));
}

/// The gear-side wire bindings match the reviewed catalog: every reason's registered status
/// (the frozen REST contract) and every operation's method and path, in catalog order.
#[test]
fn the_rest_wire_bindings_match_the_reviewed_catalog() {
    let catalog = catalog_json();
    for (reason, expected) in Reason::ALL
        .iter()
        .zip(catalog["errors"].as_array().unwrap())
    {
        assert_eq!(reason.mapping().code, expected["error_code"]);
        assert_eq!(wire::reason_status(*reason), expected["http_status"]);
        let problem = serde_json::to_value(rest::problem(OrdersError::Refused(*reason))).unwrap();
        for key in ["type", "title", "error_domain", "error_code"] {
            assert_eq!(problem[key], expected[key], "{reason:?}");
        }
        assert_eq!(problem["status"], expected["http_status"]);
    }
    let operations = catalog["operations"].as_array().unwrap();
    assert_eq!(wire::ROUTES.len(), operations.len());
    assert_eq!(OPERATIONS.len(), operations.len());
    for ((route, op), expected) in wire::ROUTES.iter().zip(OPERATIONS).zip(operations) {
        assert_eq!(route.id, op.id);
        assert_eq!(route.id, expected["id"]);
        assert_eq!(route.method, expected["method"]);
        assert_eq!(route.path, expected["path"]);
        assert_eq!(wire::route(op.id), Some(route));
    }
    assert_eq!(wire::route("invented"), None);
    // The startup checks that read the bindings: unique bindings in catalog order, the census,
    // and the targeted-operation test the PDP request shape uses.
    bss_orders::gts::validate_catalog().unwrap();
    bss_orders::authz::validate_census().unwrap();
    rest::validate_delivered().unwrap();
    let untargeted: Vec<_> = OPERATIONS
        .iter()
        .filter(|op| !bss_orders::authz::is_targeted(op))
        .map(|op| op.id)
        .collect();
    assert_eq!(untargeted, ["create", "list", "preview"]);
}

/// D-220: `OrdersError` is `#[non_exhaustive]`, so the gear's encoder ends with a fail-closed
/// wildcard arm: a disposition a newer SDK adds fails closed as the sanitized internal 500, the
/// same body `Internal` and `Integration` render. Every disposition this build knows keeps its
/// own status, and none outside those two renders as the wildcard's body.
#[test]
fn every_known_orders_error_variant_maps_to_its_own_problem() {
    let wildcard = serde_json::to_value(Problem::from(
        CanonicalError::internal("Orders internal failure").create(),
    ))
    .unwrap();
    let other = CanonicalError::unauthenticated()
        .with_reason("FUTURE_AUTH_REASON")
        .create();
    let cases = [
        (OrdersError::Refused(Reason::VersionConflict), 409),
        (
            OrdersError::Settled {
                reason: Reason::PreconditionFailed,
                diagnostics: RefusalDiagnostics::default(),
            },
            412,
        ),
        (OrdersError::Unavailable, 503),
        (OrdersError::Integration, 500),
        (OrdersError::Internal, 500),
        (OrdersError::OutcomeUnknown, 503),
        (
            OrdersError::Other {
                canonical: other.clone(),
            },
            401,
        ),
    ];
    for (error, status) in cases {
        let label = format!("{error:?}");
        let problem = rest::problem(error);
        assert_eq!(problem.status, Some(status), "{label}");
        let body = serde_json::to_value(&problem).unwrap();
        assert_eq!(body == wildcard, status == 500, "{label}");
    }
    // `Other` carries the canonical error through unchanged.
    assert_eq!(
        serde_json::to_value(rest::problem(OrdersError::Other {
            canonical: other.clone()
        }))
        .unwrap(),
        serde_json::to_value(Problem::from(other)).unwrap()
    );
}
