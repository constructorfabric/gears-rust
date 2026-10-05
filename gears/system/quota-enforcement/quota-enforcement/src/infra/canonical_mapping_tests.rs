use quota_enforcement_sdk::{
    LEASE_RESOURCE, LeaseToken, OPERATION_RESOURCE, POLICY_RESOURCE, PolicyId, QUOTA_RESOURCE,
};
use toolkit_canonical_errors::{CanonicalError, Problem};
use uuid::Uuid;

use crate::domain::error::{Dependency, DomainError, PluginKind, ResourceKind};
use crate::domain::ports::metrics::ValidationReason;
use quota_enforcement_sdk::engine::DebitPlanInvariant;

fn status(err: DomainError) -> u16 {
    Problem::from(CanonicalError::from(err))
        .status
        .expect("every canonical error carries a status")
}

#[test]
fn every_variant_family_maps_to_its_documented_status() {
    let cases: Vec<(DomainError, u16)> = vec![
        (
            DomainError::InvalidArgument {
                field: "tenant_id",
                reason: "TENANT_ID_REQUIRED",
            },
            400,
        ),
        (
            DomainError::ProjectionNotRegistered {
                projection: "gts.x~".to_owned(),
            },
            400,
        ),
        (
            DomainError::ProjectionNotResolvable {
                projection: "gts.x~".to_owned(),
            },
            400,
        ),
        (
            DomainError::CatalogInvalid {
                reason: ValidationReason::Abstract,
                subject: "gts.x~".to_owned(),
            },
            503,
        ),
        (
            DomainError::LeaseNotActive {
                token: LeaseToken::new(Uuid::from_u128(1)),
            },
            400,
        ),
        (
            DomainError::CapBelowConsumed {
                new_cap: 1,
                consumed: 2,
            },
            400,
        ),
        (DomainError::PeriodClosed, 400),
        (
            DomainError::VersionRolledBack {
                policy_id: PolicyId::global(),
                version: 2,
            },
            400,
        ),
        (DomainError::PdpDenied { reason: None }, 403),
        (
            DomainError::NotFound {
                kind: ResourceKind::Quota,
                id: "q".to_owned(),
            },
            404,
        ),
        (DomainError::IdempotencyPayloadMismatch, 409),
        (
            DomainError::VersionConflict {
                expected: 1,
                actual: 2,
            },
            409,
        ),
        (DomainError::LeaseContentionTimeout, 409),
        (DomainError::LeaseInflightLimitExceeded, 429),
        (
            DomainError::InvalidBatchItem {
                index: 1,
                field: "amount",
                reason: "INVALID_AMOUNT",
            },
            400,
        ),
        (DomainError::BulkTooLarge { items: 2, max: 1 }, 400),
        (
            DomainError::InvalidSnapshot {
                index: Some(0),
                field: "id",
                reason: "SNAPSHOT_TENANT_MISMATCH",
            },
            400,
        ),
        (DomainError::BatchTimeout, 504),
        (
            DomainError::NotReady {
                dependency: Dependency::Storage,
            },
            503,
        ),
        (DomainError::PdpUnavailable("x".to_owned()), 503),
        (DomainError::BackendUnavailable("x".to_owned()), 503),
        (DomainError::TypesRegistryUnavailable("x".to_owned()), 503),
        (
            DomainError::PluginNotFound {
                kind: PluginKind::Storage,
                vendor: "acme".to_owned(),
            },
            503,
        ),
        (
            DomainError::SchemaVersionMismatch {
                installed: 2,
                expected: 1,
            },
            500,
        ),
        (DomainError::Internal("secret detail".to_owned()), 500),
        (
            DomainError::EngineTimeout {
                engine_id: "cel".to_owned(),
            },
            504,
        ),
        (
            DomainError::EngineCostExceeded {
                engine_id: "cel".to_owned(),
            },
            429,
        ),
        (
            DomainError::InvariantViolation {
                engine_id: "cel".to_owned(),
                invariant: DebitPlanInvariant::NegativeAmount,
            },
            500,
        ),
        (
            DomainError::EngineFailure {
                engine_id: "cel".to_owned(),
                detail: "type error: request.region is 42".to_owned(),
            },
            500,
        ),
        (DomainError::CapMustBeNonNegative { cap: -1 }, 400),
        (DomainError::ThresholdsRequireBoundedCap, 400),
        (
            DomainError::ConstraintContractMismatch {
                contract: "gts.x~".to_owned(),
            },
            400,
        ),
        (
            DomainError::MetricClassificationInvalid {
                metric: "gts.m".to_owned(),
            },
            400,
        ),
        (
            DomainError::NotYetImplemented {
                feature: "rate quotas",
            },
            501,
        ),
    ];
    for (err, expected) in cases {
        let debug = format!("{err:?}");
        assert_eq!(status(err), expected, "{debug}");
    }
}

#[test]
fn not_found_carries_the_resource_type_and_name_of_its_kind() {
    let cases = vec![
        (ResourceKind::Quota, QUOTA_RESOURCE),
        (ResourceKind::Policy, POLICY_RESOURCE),
        (ResourceKind::Lease, LEASE_RESOURCE),
        (ResourceKind::Operation, OPERATION_RESOURCE),
    ];
    for (kind, resource_type) in cases {
        let err = CanonicalError::from(DomainError::NotFound {
            kind,
            id: "abc".to_owned(),
        });
        let CanonicalError::NotFound { .. } = &err else {
            panic!("expected NotFound for {kind}, got {err:?}");
        };
        let rendered = serde_json::to_string(&Problem::from(err)).expect("problem json");
        assert!(rendered.contains(resource_type), "{kind}: {rendered}");
        assert!(rendered.contains("abc"), "{kind}: {rendered}");
    }
}

#[test]
fn permission_denied_and_internal_errors_leak_no_detail() {
    let denied = Problem::from(CanonicalError::from(DomainError::PdpDenied {
        reason: Some("pdp said: subject lacks role X".to_owned()),
    }));
    let rendered = serde_json::to_string(&denied).expect("json");
    assert!(
        !rendered.contains("subject lacks role"),
        "PDP detail stays in logs: {rendered}"
    );
    assert!(rendered.contains("AUTHZ"), "{rendered}");

    let internal = Problem::from(CanonicalError::from(DomainError::Internal(
        "connection string postgres://user:pw@host".to_owned(),
    )));
    let rendered = serde_json::to_string(&internal).expect("json");
    assert!(!rendered.contains("postgres://"), "{rendered}");
}

#[test]
fn precondition_and_abort_envelopes_carry_their_upper_snake_tokens() {
    let precondition = Problem::from(CanonicalError::from(DomainError::CapBelowConsumed {
        new_cap: 5,
        consumed: 9,
    }));
    let rendered = serde_json::to_string(&precondition).expect("json");
    assert!(rendered.contains("CAP_BELOW_CONSUMED"), "{rendered}");

    let aborted = Problem::from(CanonicalError::from(
        DomainError::IdempotencyPayloadMismatch,
    ));
    let rendered = serde_json::to_string(&aborted).expect("json");
    assert!(
        rendered.contains("IDEMPOTENCY_PAYLOAD_MISMATCH"),
        "{rendered}"
    );

    let exhausted = Problem::from(CanonicalError::from(
        DomainError::LeaseInflightLimitExceeded,
    ));
    let rendered = serde_json::to_string(&exhausted).expect("json");
    assert!(
        rendered.contains("LEASE_INFLIGHT_LIMIT_EXCEEDED"),
        "{rendered}"
    );
}

#[test]
fn unavailability_names_the_dependency_class_but_not_the_cause() {
    let rendered = serde_json::to_string(&Problem::from(CanonicalError::from(
        DomainError::BackendUnavailable("host 10.0.0.7 refused".to_owned()),
    )))
    .expect("json");
    assert!(rendered.contains("DEPENDENCY_UNAVAILABLE"), "{rendered}");
    assert!(!rendered.contains("10.0.0.7"), "{rendered}");
}

#[test]
fn not_yet_implemented_is_unimplemented_with_the_token_leading_the_detail() {
    let problem = Problem::from(CanonicalError::from(DomainError::NotYetImplemented {
        feature: "rate quotas",
    }));
    assert_eq!(problem.status, Some(501));
    let detail = problem.detail.clone();
    assert!(
        detail.starts_with("NOT_YET_IMPLEMENTED:"),
        "the token leads the detail: {detail}"
    );
    let rendered = serde_json::to_string(&problem).expect("json");
    assert!(rendered.contains(QUOTA_RESOURCE), "{rendered}");
}

#[test]
fn batch_rejections_name_the_item_and_lead_with_their_tokens() {
    let item = serde_json::to_string(&Problem::from(CanonicalError::from(
        DomainError::InvalidBatchItem {
            index: 3,
            field: "amount",
            reason: "INVALID_AMOUNT",
        },
    )))
    .expect("json");
    assert!(item.contains("items[3].amount"), "{item}");
    assert!(item.contains("INVALID_AMOUNT"), "{item}");
    let size = serde_json::to_string(&Problem::from(CanonicalError::from(
        DomainError::BulkTooLarge { items: 2, max: 1 },
    )))
    .expect("json");
    assert!(size.contains("BULK_TOO_LARGE"), "{size}");
    let timeout = Problem::from(CanonicalError::from(DomainError::BatchTimeout));
    assert!(
        timeout.detail.starts_with("BATCH_TIMEOUT:"),
        "the token leads the detail: {}",
        timeout.detail
    );
}

#[test]
fn snapshot_rejections_name_the_subject_or_the_request_field() {
    let subject = serde_json::to_string(&Problem::from(CanonicalError::from(
        DomainError::InvalidSnapshot {
            index: Some(2),
            field: "kind",
            reason: "SUBJECT_KIND_INVALID",
        },
    )))
    .expect("json");
    assert!(subject.contains("subjects[2].kind"), "{subject}");
    assert!(subject.contains("SUBJECT_KIND_INVALID"), "{subject}");
    let request = serde_json::to_string(&Problem::from(CanonicalError::from(
        DomainError::InvalidSnapshot {
            index: None,
            field: "limit",
            reason: "SNAPSHOT_LIMIT_OUT_OF_RANGE",
        },
    )))
    .expect("json");
    assert!(request.contains("\"limit\""), "{request}");
    assert!(
        request.contains(QUOTA_RESOURCE),
        "a snapshot reads Quotas: {request}"
    );
}

#[test]
fn quota_lifecycle_rejections_carry_their_tokens_and_subjects() {
    let cases = vec![
        (
            DomainError::CapMustBeNonNegative { cap: -5 },
            "CAP_MUST_BE_NON_NEGATIVE",
            "cap",
        ),
        (
            DomainError::ThresholdsRequireBoundedCap,
            "THRESHOLDS_REQUIRE_BOUNDED_CAP",
            "notification_thresholds",
        ),
        (
            DomainError::ConstraintContractMismatch {
                contract: "gts.cf.core.qe.constraint.v1~x.y.z.w.v1~".to_owned(),
            },
            "CONSTRAINT_CONTRACT_MISMATCH",
            "metadata",
        ),
        (
            DomainError::MetricClassificationInvalid {
                metric: "gts.cf.core.qe.metric_type.v1~cf.test.qe.m.v1".to_owned(),
            },
            "METRIC_CLASSIFICATION_INVALID",
            "gts.cf.core.qe.metric_type.v1~cf.test.qe.m.v1",
        ),
    ];
    for (err, token, subject) in cases {
        let rendered =
            serde_json::to_string(&Problem::from(CanonicalError::from(err))).expect("json");
        assert!(rendered.contains(token), "{token}: {rendered}");
        assert!(rendered.contains(subject), "{subject}: {rendered}");
    }
}

#[test]
fn engine_failures_map_to_their_canonical_classes_and_leak_no_engine_detail() {
    // The toolkit's `Internal` envelope carries a fixed detail on the wire, so
    // the `INVARIANT_VIOLATION` sub-token is observable on the domain error,
    // the log line and the `invariant` metric label, not in the HTTP body.
    let violation = DomainError::InvariantViolation {
        engine_id: "cel".to_owned(),
        invariant: DebitPlanInvariant::QuotaIdOutsideApplicableSet,
    };
    assert!(
        violation
            .to_string()
            .contains("quota_id_outside_applicable_set")
    );
    let problem = Problem::from(CanonicalError::from(violation));
    assert_eq!(problem.status, Some(500));

    // What the engine said may quote operator config or request values; the
    // response says only that an internal error occurred.
    let failure = Problem::from(CanonicalError::from(DomainError::EngineFailure {
        engine_id: "cel".to_owned(),
        detail: "type error near request.region == \"eu-secret\"".to_owned(),
    }));
    let rendered = serde_json::to_string(&failure).expect("json");
    assert_eq!(failure.status, Some(500));
    assert!(!rendered.contains("eu-secret"), "{rendered}");

    let timeout = Problem::from(CanonicalError::from(DomainError::EngineTimeout {
        engine_id: "cel".to_owned(),
    }));
    assert_eq!(
        timeout.status,
        Some(504),
        "Timeout lifts to DeadlineExceeded"
    );
    let cost = Problem::from(CanonicalError::from(DomainError::EngineCostExceeded {
        engine_id: "cel".to_owned(),
    }));
    assert_eq!(
        cost.status,
        Some(429),
        "CostExceeded lifts to ResourceExhausted"
    );
    assert!(
        serde_json::to_string(&cost)
            .expect("json")
            .contains("ENGINE_COST_EXCEEDED")
    );
}

#[test]
fn a_bulk_item_error_keeps_its_category_and_names_the_item() {
    let field = CanonicalError::from(
        DomainError::InvalidArgument {
            field: "cap",
            reason: "CAP_MUST_BE_NON_NEGATIVE",
        }
        .at_item(3),
    );
    assert_eq!(field.status_code(), 400);
    let rendered = serde_json::to_string(&Problem::from(field)).expect("json");
    assert!(rendered.contains("items[3].cap"), "{rendered}");

    let precondition = CanonicalError::from(
        DomainError::CapBelowConsumed {
            new_cap: 5,
            consumed: 9,
        }
        .at_item(1),
    );
    assert_eq!(
        precondition.status_code(),
        status(DomainError::CapBelowConsumed {
            new_cap: 5,
            consumed: 9
        })
    );
    let rendered = serde_json::to_string(&Problem::from(precondition)).expect("json");
    assert!(rendered.contains("items[1].cap"), "{rendered}");
    assert!(rendered.contains("CAP_BELOW_CONSUMED"), "{rendered}");

    let missing = CanonicalError::from(
        DomainError::NotFound {
            kind: ResourceKind::Quota,
            id: Uuid::nil().to_string(),
        }
        .at_item(0),
    );
    assert_eq!(missing.status_code(), 404);
    assert!(
        missing.detail().starts_with("items[0]: "),
        "{}",
        missing.detail()
    );

    let internal = CanonicalError::from(DomainError::Internal("secret".to_owned()).at_item(2));
    assert!(
        !internal.detail().contains("items[2]") && !internal.detail().contains("secret"),
        "an internal error stays opaque: {}",
        internal.detail()
    );
}

/// The variants the status table does not reach: each keeps its documented
/// status and names its own precondition token or dependency on the wire.
#[test]
fn the_remaining_variants_map_to_their_status_and_their_own_token() {
    let cases: Vec<(DomainError, u16, &str)> = vec![
        (
            DomainError::OverCommitNotAuthorized {
                reserved: 5,
                actual: 9,
            },
            400,
            "OVER_COMMIT_NOT_AUTHORIZED",
        ),
        (
            DomainError::MetricNotRegistered {
                metric: "gts.m".to_owned(),
            },
            400,
            "METRIC_NOT_REGISTERED",
        ),
        (
            DomainError::MetricNotQuotaGated {
                metric: "gts.m".to_owned(),
            },
            400,
            "METRIC_NOT_QUOTA_GATED",
        ),
        (
            DomainError::PolicyScopeOccupied {
                scope: quota_enforcement_sdk::PolicyScope::Global,
            },
            409,
            "POLICY_SCOPE_OCCUPIED",
        ),
        (
            DomainError::InvalidPluginInstance {
                kind: PluginKind::Storage,
                gts_id: "gts.x".to_owned(),
                reason: "bad".to_owned(),
            },
            503,
            "plugin unavailable",
        ),
        (
            DomainError::PluginClientNotRegistered {
                kind: PluginKind::Storage,
                gts_id: "gts.x".to_owned(),
            },
            503,
            "plugin unavailable",
        ),
        (
            DomainError::ClusterUnavailable("down".to_owned()),
            503,
            "cluster unavailable",
        ),
    ];
    for (err, expected, token) in cases {
        let label = format!("{err:?}");
        let problem = Problem::from(CanonicalError::from(err));
        assert_eq!(problem.status, Some(expected), "{label}");
        let body = serde_json::to_string(&problem).expect("problem serializes");
        assert!(body.contains(token), "{label}: {body}");
    }
}
