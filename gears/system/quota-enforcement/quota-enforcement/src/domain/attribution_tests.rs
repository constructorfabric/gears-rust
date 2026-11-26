#![allow(clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use gts::GtsTypeId;
use quota_enforcement_sdk::{EvaluationAttribution, MetricRef, SubjectClaim, SubjectRef, TenantId};
use serde_json::{Map, Value, json};
use toolkit_security::pep_properties;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

use super::{AdmittedEvaluation, Attribution};
use crate::domain::admission::Admission;
use crate::domain::catalog::MetricCatalog;
use crate::domain::error::DomainError;
use crate::domain::pep::{actions, properties, resources};
use crate::domain::ports::metrics::{DenialReason, ValidationReason, ValidationSurface};
use crate::domain::tokens;
use crate::test_support::{
    DEVICE_SUBJECT_TYPE, DenyAllPdp, METRIC_OTHER, METRIC_REQUESTS, METRIC_TOKENS,
    PermitTenantsPdp, RecordingMetrics, TENANT_SUBJECT_TYPE, TupleMatchingPdp, USER_SUBJECT_TYPE,
    caller, catalog_over, fail_registry_reads, metric_type_document, platform_registry,
    registry_reads, subject_type_document, tenant,
};

const ADMIN_SUBJECT_TYPE: &str =
    "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~acme.core.quota.admin.v1~";

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

struct Harness {
    registry: Arc<MockTypesRegistry>,
    catalog: Arc<MetricCatalog>,
    admission: Admission,
    metrics: Arc<RecordingMetrics>,
}

impl Harness {
    fn with_pdp(pdp: Arc<dyn AuthZResolverApi>) -> Self {
        let registry = platform_registry();
        let metrics = Arc::new(RecordingMetrics::default());
        let catalog = catalog_over(&registry, metrics.clone());
        Self {
            registry,
            catalog,
            admission: Admission::new(PolicyEnforcer::new(pdp), metrics.clone()),
            metrics,
        }
    }

    async fn admit(
        &self,
        attribution: EvaluationAttribution,
    ) -> Result<AdmittedEvaluation, DomainError> {
        let admission = self.admission.pdp(caller());
        Attribution::new(&admission, &self.catalog, self.metrics.as_ref())
            .admit_evaluation(&resources::OPERATION, actions::DEBIT, attribution)
            .await
    }
}

fn claim(subject_type: &str, id: &str) -> SubjectClaim {
    SubjectClaim {
        subject_type: subject_type.to_owned(),
        id: id.to_owned(),
    }
}

fn user(id: &str) -> SubjectClaim {
    claim(USER_SUBJECT_TYPE, id)
}

fn metric(metric_type: &str, metadata: Value) -> MetricRef {
    let Value::Object(metadata) = metadata else {
        panic!("metadata is an object");
    };
    MetricRef {
        metric_type: metric_type.to_owned(),
        id: None,
        metadata: Some(metadata),
    }
}

fn request() -> EvaluationAttribution {
    EvaluationAttribution {
        tenant_id: tenant(),
        subjects: vec![user("u-1")],
        metric: metric(METRIC_TOKENS, json!({ "region": "eu-west-1" })),
    }
}

fn subject(subject_type: &str, id: &str) -> SubjectRef {
    SubjectRef {
        subject_type: GtsTypeId::new(subject_type),
        subject_id: id.to_owned(),
    }
}

fn tenant_subject() -> SubjectRef {
    SubjectRef::tenant(tenant())
}

fn permitting() -> Arc<PermitTenantsPdp> {
    Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))
}

fn invalid(field: &'static str, reason: &'static str) -> DomainError {
    DomainError::InvalidArgument { field, reason }
}

#[tokio::test]
async fn a_conforming_request_adds_the_tenant_subject_before_the_named_ones() {
    let pdp = permitting();
    let h = Harness::with_pdp(pdp.clone());
    let admitted = h.admit(request()).await.expect("admitted");

    assert_eq!(
        admitted.attribution.subjects,
        vec![tenant_subject(), subject(USER_SUBJECT_TYPE, "u-1")]
    );
    assert_eq!(admitted.attribution.tenant_id, tenant());
    assert!(
        admitted
            .attribution
            .access_scope
            .contains_uuid(pep_properties::OWNER_TENANT_ID, tenant().as_uuid())
    );
    assert_eq!(admitted.metric.id().as_str(), METRIC_TOKENS);
    assert_eq!(admitted.metric_id, None);
    assert_eq!(admitted.request, object(&json!({ "region": "eu-west-1" })));
    assert_eq!(pdp.calls(), 1, "exactly one PDP round trip");
    assert!(h.metrics.denials().is_empty());
    assert!(h.metrics.contract_failures().is_empty());
    assert!(h.metrics.admitted_violations().is_empty());
}

#[tokio::test]
async fn naming_the_tenant_subject_yields_the_same_subject_set() {
    let h = Harness::with_pdp(permitting());
    let plain = h.admit(request()).await.expect("plain");
    let named = h
        .admit(EvaluationAttribution {
            subjects: vec![
                claim(TENANT_SUBJECT_TYPE, &tenant().to_string()),
                user("u-1"),
            ],
            ..request()
        })
        .await
        .expect("named tenant");
    assert_eq!(named.attribution.subjects, plain.attribution.subjects);
    assert_eq!(named.authorized, plain.authorized);

    let tenant_only = h
        .admit(EvaluationAttribution {
            subjects: vec![claim(TENANT_SUBJECT_TYPE, &tenant().to_string())],
            ..request()
        })
        .await
        .expect("a request that charges only the tenant names it");
    assert_eq!(tenant_only.attribution.subjects, vec![tenant_subject()]);
}

#[tokio::test]
async fn the_pdp_receives_the_complete_tuple_with_the_target_tenant() {
    let pdp = permitting();
    let h = Harness::with_pdp(pdp.clone());
    let with_id = EvaluationAttribution {
        metric: MetricRef {
            id: Some("gpt-4o".to_owned()),
            ..metric(METRIC_TOKENS, json!({ "region": "eu" }))
        },
        ..request()
    };
    let admitted = h.admit(with_id).await.expect("admitted");
    assert_eq!(admitted.metric_id.as_deref(), Some("gpt-4o"));

    let resource = pdp.last_resource().expect("the PDP saw a resource");
    assert_eq!(
        resource.properties.get(pep_properties::OWNER_TENANT_ID),
        Some(&Value::String(tenant().as_uuid().to_string()))
    );
    assert_eq!(
        resource.properties.get(properties::METRIC),
        Some(&Value::String(METRIC_TOKENS.to_owned()))
    );
    assert_eq!(
        resource.properties.get(properties::METRIC_ID),
        Some(&json!("gpt-4o"))
    );
    assert_eq!(
        resource.properties.get(properties::SUBJECTS),
        Some(&json!([
            { "type": TENANT_SUBJECT_TYPE, "id": tenant().to_string() },
            { "type": USER_SUBJECT_TYPE, "id": "u-1" },
        ]))
    );
}

#[tokio::test]
async fn malformed_shape_is_rejected_before_the_pdp_with_a_stable_token() {
    let pdp = permitting();
    let h = Harness::with_pdp(pdp.clone());

    let cases: Vec<(&str, EvaluationAttribution, DomainError)> = vec![
        (
            "nil tenant",
            EvaluationAttribution {
                tenant_id: TenantId::new(Uuid::nil()),
                ..request()
            },
            invalid("tenant_id", tokens::TENANT_ID_REQUIRED),
        ),
        (
            "metric under another base",
            EvaluationAttribution {
                metric: metric(USER_SUBJECT_TYPE, json!({})),
                ..request()
            },
            invalid("metric.type", tokens::METRIC_INVALID),
        ),
        (
            "malformed metric",
            EvaluationAttribution {
                metric: metric("tokens", json!({})),
                ..request()
            },
            invalid("metric.type", tokens::METRIC_INVALID),
        ),
        (
            "empty metric id",
            EvaluationAttribution {
                metric: MetricRef {
                    id: Some(String::new()),
                    ..metric(METRIC_TOKENS, json!({}))
                },
                ..request()
            },
            invalid("metric.id", tokens::METRIC_ID_INVALID),
        ),
        (
            "metric id longer than the bound",
            EvaluationAttribution {
                metric: MetricRef {
                    id: Some("m".repeat(257)),
                    ..metric(METRIC_TOKENS, json!({}))
                },
                ..request()
            },
            invalid("metric.id", tokens::METRIC_ID_TOO_LONG),
        ),
        (
            "no subject",
            EvaluationAttribution {
                subjects: Vec::new(),
                ..request()
            },
            invalid("subjects", tokens::SUBJECTS_REQUIRED),
        ),
        (
            "too many subjects",
            EvaluationAttribution {
                subjects: vec![request().subjects[0].clone(); 17],
                ..request()
            },
            invalid("subjects", tokens::SUBJECTS_TOO_MANY),
        ),
        (
            "empty subject id",
            EvaluationAttribution {
                subjects: vec![user("  ")],
                ..request()
            },
            invalid("subjects", tokens::SUBJECT_ID_REQUIRED),
        ),
        (
            "subject id longer than a Quota can hold",
            EvaluationAttribution {
                subjects: vec![user(&"u".repeat(257))],
                ..request()
            },
            invalid("subjects", tokens::SUBJECT_ID_TOO_LONG),
        ),
        (
            "subject type outside the subject base",
            EvaluationAttribution {
                subjects: vec![claim(METRIC_TOKENS, "u-1")],
                ..request()
            },
            invalid("subjects", tokens::SUBJECT_TYPE_INVALID),
        ),
        (
            "two subjects of one type",
            EvaluationAttribution {
                subjects: vec![user("u-1"), user("u-2")],
                ..request()
            },
            invalid("subjects", tokens::SUBJECT_TYPE_DUPLICATE),
        ),
        (
            "tenant subject of another tenant",
            EvaluationAttribution {
                subjects: vec![claim(
                    TENANT_SUBJECT_TYPE,
                    &Uuid::from_u128(0xbad).to_string(),
                )],
                ..request()
            },
            invalid("subjects", tokens::TENANT_SUBJECT_MISMATCH),
        ),
        (
            "metadata absent",
            EvaluationAttribution {
                metric: MetricRef {
                    metadata: None,
                    ..metric(METRIC_TOKENS, json!({}))
                },
                ..request()
            },
            invalid("metric.metadata", tokens::METADATA_REQUIRED),
        ),
    ];
    let expected_count = cases.len();
    for (name, attribution, expected) in cases {
        let err = h.admit(attribution).await.expect_err(name);
        assert_eq!(err, expected, "{name}");
    }
    assert_eq!(pdp.calls(), 0, "shape checks run before the PDP");
    assert_eq!(
        registry_reads(&h.registry),
        0,
        "and before any registry read"
    );
    assert_eq!(
        h.metrics.denials(),
        vec![DenialReason::InvalidArgument; expected_count]
    );
    let failures = h.metrics.contract_failures();
    assert_eq!(failures.len(), expected_count);
    assert!(
        failures
            .iter()
            .all(|(surface, _)| *surface == ValidationSurface::CallerAttribution)
    );
    assert_eq!(
        failures
            .iter()
            .filter(|(_, reason)| *reason == ValidationReason::MetadataMissing)
            .count(),
        1
    );
}

#[tokio::test]
async fn a_pdp_denial_precedes_any_catalogue_lookup() {
    let h = Harness::with_pdp(Arc::new(DenyAllPdp));
    let unregistered = EvaluationAttribution {
        metric: metric(METRIC_OTHER, json!({})),
        ..request()
    };
    let err = h.admit(unregistered).await.expect_err("denied");
    assert!(matches!(err, DomainError::PdpDenied { .. }), "{err:?}");
    assert_eq!(registry_reads(&h.registry), 0, "no catalogue lookup ran");
    assert_eq!(h.metrics.denials(), vec![DenialReason::PermissionDenied]);
}

#[tokio::test]
async fn an_unauthorized_tuple_is_denied_before_evaluation() {
    let expected = object(&json!({
        pep_properties::OWNER_TENANT_ID: tenant().as_uuid().to_string(),
        properties::METRIC: METRIC_TOKENS,
        properties::SUBJECTS: [
            { "type": TENANT_SUBJECT_TYPE, "id": tenant().to_string() },
            { "type": USER_SUBJECT_TYPE, "id": "u-1" },
        ],
    }));
    let pdp = Arc::new(TupleMatchingPdp::new(expected, vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone());

    h.admit(request()).await.expect("the authorized tuple");

    let other_subject = EvaluationAttribution {
        subjects: vec![user("u-2")],
        ..request()
    };
    assert!(matches!(
        h.admit(other_subject).await,
        Err(DomainError::PdpDenied { .. })
    ));
    let other_metric = EvaluationAttribution {
        metric: metric(METRIC_REQUESTS, json!({})),
        ..request()
    };
    assert!(matches!(
        h.admit(other_metric).await,
        Err(DomainError::PdpDenied { .. })
    ));
    let other_tenant = EvaluationAttribution {
        tenant_id: TenantId::new(Uuid::from_u128(0xbad)),
        ..request()
    };
    assert!(matches!(
        h.admit(other_tenant).await,
        Err(DomainError::PdpDenied { .. })
    ));
    assert_eq!(pdp.calls(), 4);
    assert!(h.metrics.admitted_violations().is_empty());
}

#[tokio::test]
async fn an_exact_or_derived_admitted_type_is_admitted_and_another_is_not() {
    let h = Harness::with_pdp(permitting());
    h.registry.seed(
        ADMIN_SUBJECT_TYPE,
        subject_type_document(ADMIN_SUBJECT_TYPE),
    );

    h.admit(request()).await.expect("exact type");
    let derived = h
        .admit(EvaluationAttribution {
            subjects: vec![claim(ADMIN_SUBJECT_TYPE, "a-1")],
            ..request()
        })
        .await
        .expect("a type derived from an admitted one");
    assert_eq!(
        derived.attribution.subjects,
        vec![tenant_subject(), subject(ADMIN_SUBJECT_TYPE, "a-1")]
    );

    let err = h
        .admit(EvaluationAttribution {
            subjects: vec![claim(DEVICE_SUBJECT_TYPE, "d-1")],
            ..request()
        })
        .await
        .expect_err("the metric does not admit devices");
    assert_eq!(err, invalid("subjects", tokens::METRIC_NOT_ADMITTED));
    assert_eq!(
        h.metrics.admitted_violations(),
        vec![ValidationSurface::RequestSubject]
    );
}

#[tokio::test]
async fn a_metric_admitting_the_subject_base_admits_every_subject_type() {
    let h = Harness::with_pdp(permitting());
    let admitted = h
        .admit(EvaluationAttribution {
            subjects: vec![user("u-1"), claim(DEVICE_SUBJECT_TYPE, "d-1")],
            metric: metric(METRIC_REQUESTS, json!({})),
            ..request()
        })
        .await
        .expect("admitted");
    assert_eq!(
        admitted.attribution.subjects,
        vec![
            tenant_subject(),
            subject(DEVICE_SUBJECT_TYPE, "d-1"),
            subject(USER_SUBJECT_TYPE, "u-1"),
        ]
    );
}

#[tokio::test]
async fn the_tenant_type_is_admitted_by_the_empty_default() {
    let h = Harness::with_pdp(permitting());
    let tenant_only = "gts.cf.core.quota.metric.v1~cf.qetest.metric.tenant_only.v1~";
    h.registry.seed(
        tenant_only,
        metric_type_document(tenant_only, json!({ "kind": "counter" }), json!({})),
    );
    h.admit(EvaluationAttribution {
        subjects: vec![claim(TENANT_SUBJECT_TYPE, &tenant().to_string())],
        metric: metric(tenant_only, json!({})),
        ..request()
    })
    .await
    .expect("the tenant type is always admitted");
    let err = h
        .admit(EvaluationAttribution {
            metric: metric(tenant_only, json!({})),
            ..request()
        })
        .await
        .expect_err("users are not admitted");
    assert_eq!(err, invalid("subjects", tokens::METRIC_NOT_ADMITTED));
}

#[tokio::test]
async fn an_unregistered_subject_type_is_rejected_after_admission() {
    let h = Harness::with_pdp(permitting());
    let err = h
        .admit(EvaluationAttribution {
            subjects: vec![claim(
                "gts.cf.core.quota.subj.v1~acme.core.quota.robot.v1~",
                "r",
            )],
            metric: metric(METRIC_REQUESTS, json!({})),
            ..request()
        })
        .await
        .expect_err("unregistered");
    assert!(
        matches!(err, DomainError::ProjectionNotRegistered { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_metric_object_is_validated_against_its_metric_type() {
    let h = Harness::with_pdp(permitting());

    h.admit(EvaluationAttribution {
        metric: MetricRef {
            id: Some("gpt-4o".to_owned()),
            ..metric(METRIC_TOKENS, json!({ "region": "eu", "model": "gpt" }))
        },
        ..request()
    })
    .await
    .expect("conforms");

    for metadata in [json!({ "region": 7 }), json!({ "temperature": 0.7 })] {
        let err = h
            .admit(EvaluationAttribution {
                metric: metric(METRIC_TOKENS, metadata.clone()),
                ..request()
            })
            .await
            .expect_err("does not fit");
        assert_eq!(
            err,
            invalid("metric", tokens::CONTRACT_VIOLATION),
            "{metadata}"
        );
    }
    assert_eq!(
        h.metrics.contract_failures(),
        vec![
            (
                ValidationSurface::RequestMetric,
                ValidationReason::SchemaViolation
            );
            2
        ]
    );
}

#[tokio::test]
async fn an_attribute_its_owner_just_added_to_the_metric_type_is_accepted_before_the_schedule() {
    let h = Harness::with_pdp(permitting());
    let with_temperature = || EvaluationAttribution {
        metric: metric(METRIC_TOKENS, json!({ "temperature": "low" })),
        ..request()
    };
    h.admit(request()).await.expect("the metric is held");

    h.registry.seed_entity(
        types_registry_sdk::testing_platform::Seed::new(
            METRIC_TOKENS,
            crate::test_support::metric_type_document(
                METRIC_TOKENS,
                json!({ "kind": "counter", "admitted_subjects": [USER_SUBJECT_TYPE] }),
                json!({ "temperature": { "type": "string" } }),
            ),
        )
        .version(2),
    );

    h.admit(with_temperature())
        .await
        .expect("the revision is read on the rejection");
}

#[tokio::test]
async fn a_revision_read_on_a_rejection_that_no_longer_admits_the_subject_type_refuses_it() {
    let h = Harness::with_pdp(permitting());
    h.admit(request()).await.expect("the metric is held");

    h.registry.seed_entity(
        types_registry_sdk::testing_platform::Seed::new(
            METRIC_TOKENS,
            crate::test_support::metric_type_document(
                METRIC_TOKENS,
                json!({ "kind": "counter", "admitted_subjects": [DEVICE_SUBJECT_TYPE] }),
                json!({ "temperature": { "type": "string" } }),
            ),
        )
        .version(2),
    );

    let refused = h
        .admit(EvaluationAttribution {
            metric: metric(METRIC_TOKENS, json!({ "temperature": "low" })),
            ..request()
        })
        .await
        .expect_err("the revision does not admit the user subject type");
    assert_eq!(
        refused,
        DomainError::InvalidArgument {
            field: "subjects",
            reason: tokens::METRIC_NOT_ADMITTED,
        }
    );
}

#[tokio::test]
async fn an_unregistered_abstract_or_underived_metric_type_is_refused() {
    let h = Harness::with_pdp(permitting());
    let unregistered = h
        .admit(EvaluationAttribution {
            metric: metric(METRIC_OTHER, json!({})),
            ..request()
        })
        .await
        .expect_err("unregistered");
    assert!(
        matches!(unregistered, DomainError::MetricNotRegistered { .. }),
        "{unregistered:?}"
    );

    let family = "gts.cf.core.quota.metric.v1~cf.qetest.metric.family.v1~";
    let mut abstract_type = metric_type_document(family, json!({ "kind": "counter" }), json!({}));
    abstract_type["x-gts-abstract"] = json!(true);
    h.registry.seed(family, abstract_type);
    let unavailable = h
        .admit(EvaluationAttribution {
            metric: metric(family, json!({})),
            ..request()
        })
        .await
        .expect_err("abstract");
    assert!(
        matches!(unavailable, DomainError::ProjectionNotResolvable { .. }),
        "{unavailable:?}"
    );

    let underived = h
        .admit(EvaluationAttribution {
            metric: metric("gts.cf.qetest.metrics.tokens.v1~", json!({})),
            ..request()
        })
        .await
        .expect_err("not derived from the metric base");
    assert_eq!(underived, invalid("metric.type", tokens::METRIC_INVALID));
}

#[tokio::test]
async fn a_held_metric_needs_no_registry_call_and_a_first_use_needs_the_registry() {
    let h = Harness::with_pdp(permitting());
    h.admit(request()).await.expect("first use reads");
    let reads = registry_reads(&h.registry);
    fail_registry_reads(&h.registry);

    h.admit(request())
        .await
        .expect("the catalogue answers with the registry down");
    assert_eq!(registry_reads(&h.registry), reads);

    let err = h
        .admit(EvaluationAttribution {
            metric: metric(METRIC_REQUESTS, json!({})),
            ..request()
        })
        .await
        .expect_err("first use while the registry is down");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_reversal_is_admitted_without_metadata_or_a_catalogue_check() {
    let h = Harness::with_pdp(permitting());
    let admitted = h.admit(request()).await.expect("debit");
    fail_registry_reads(&h.registry);

    let admission = h.admission.pdp(caller());
    let reversal = Attribution::new(&admission, &h.catalog, h.metrics.as_ref())
        .admit_reversal(
            &resources::OPERATION,
            actions::ROLLBACK,
            EvaluationAttribution {
                metric: MetricRef {
                    metadata: None,
                    ..metric(METRIC_TOKENS, json!({}))
                },
                ..request()
            },
        )
        .await
        .expect("no metadata and no catalogue check");
    assert_eq!(reversal.attribution.subjects, admitted.attribution.subjects);
    assert_eq!(reversal.authorized, admitted.authorized);
}

#[tokio::test]
async fn the_attribution_digest_is_the_fixed_form_of_what_was_admitted() {
    let h = Harness::with_pdp(permitting());
    let admitted = h.admit(request()).await.expect("admitted");
    let other_metadata = h
        .admit(EvaluationAttribution {
            metric: metric(METRIC_TOKENS, json!({ "region": "us-east-1" })),
            ..request()
        })
        .await
        .expect("admitted");

    assert_eq!(
        admitted.authorized,
        quota_enforcement_sdk::AttributionDigest::of(
            tenant(),
            &[subject(USER_SUBJECT_TYPE, "u-1")],
            admitted.metric.id(),
            None,
        )
    );
    assert_eq!(
        other_metadata.authorized, admitted.authorized,
        "the metric metadata is not part of the digest"
    );
}

#[tokio::test]
async fn with_the_registry_down_a_subject_type_not_admitted_is_invalid() {
    let h = Harness::with_pdp(permitting());
    h.admit(request()).await.expect("holds the metric");
    fail_registry_reads(&h.registry);

    let err = h
        .admit(EvaluationAttribution {
            subjects: vec![claim(DEVICE_SUBJECT_TYPE, "d-1")],
            ..request()
        })
        .await
        .expect_err("the metric does not admit devices");

    assert_eq!(err, invalid("subjects", tokens::METRIC_NOT_ADMITTED));
}

#[tokio::test]
async fn with_the_registry_down_an_unavailable_metric_is_a_failed_precondition() {
    let h = Harness::with_pdp(permitting());
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";
    h.registry.seed(broken, {
        let mut document = metric_type_document(broken, json!({ "kind": "counter" }), json!({}));
        document["x-gts-abstract"] = json!(true);
        document
    });
    let attribution = || EvaluationAttribution {
        subjects: vec![claim(DEVICE_SUBJECT_TYPE, "d-1")],
        metric: metric(broken, json!({})),
        ..request()
    };
    h.catalog
        .served(
            &quota_enforcement_sdk::MetricId::parse(broken).expect("metric"),
            ValidationSurface::RequestMetric,
        )
        .await
        .expect_err("holds the unavailable entry");
    fail_registry_reads(&h.registry);

    let err = h.admit(attribution()).await.expect_err("unavailable");

    assert!(
        matches!(err, DomainError::ProjectionNotResolvable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_held_metric_reads_the_subject_types_not_cached_with_one_read() {
    let h = Harness::with_pdp(permitting());
    let requests = quota_enforcement_sdk::MetricId::parse(METRIC_REQUESTS).expect("metric");
    h.catalog
        .served(&requests, ValidationSurface::RequestMetric)
        .await
        .expect("held");
    let reads = registry_reads(&h.registry);

    h.admit(EvaluationAttribution {
        subjects: vec![user("u-1"), claim(DEVICE_SUBJECT_TYPE, "d-1")],
        metric: metric(METRIC_REQUESTS, json!({})),
        ..request()
    })
    .await
    .expect("admitted");

    assert_eq!(registry_reads(&h.registry), reads + 1);
}
