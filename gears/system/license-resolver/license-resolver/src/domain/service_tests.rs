//! Unit tests for the license resolver service: validation ordering, plugin
//! delegation, fail-closed behaviour and telemetry.

use license_resolver_sdk::{FieldViolation, LicenseResolverError};
use serde_json::json;

use super::*;
use crate::domain::ContractRegistry;
use crate::domain::principal::PrincipalMapper;
use crate::domain::test_support::{
    FailureKind, FakeContractRegistry, MODEL_USAGE_RESOURCE, MockPlugin, OPERATOR_PRINCIPAL,
    RecordingMetrics, SEAT_SUBJECT, USER_PRINCIPAL, USER_SUBJECT, conforming_request,
    counting_hub_with_registry_only, empty_hub, hub_with_registry_and_plugin,
    hub_without_plugin_instances, model_usage, principal_mapping, request, resource, subject,
    test_context, test_instance_id, test_tenant,
};
use crate::domain::validation::ContractValidator;
use uuid::Uuid;

/// Principal type mapped to a Subject contract whose schema requires metadata.
const METADATA_PRINCIPAL: &str = "needs-metadata";

const VENDOR: &str = "constructorfabric";

fn service_with(
    hub: Arc<ClientHub>,
    registry: FakeContractRegistry,
    metrics: Arc<RecordingMetrics>,
) -> Service {
    let registry: Arc<dyn ContractRegistry> = Arc::new(registry);
    Service::new(
        hub,
        VENDOR.to_owned(),
        ContractValidator::new(registry.clone()),
        PrincipalMapper::new(
            principal_mapping(&[
                (USER_PRINCIPAL, SEAT_SUBJECT),
                (METADATA_PRINCIPAL, USER_SUBJECT),
            ]),
            registry,
        ),
        metrics as Arc<dyn LicenseMetrics>,
    )
}

/// Service whose contracts all resolve, wired to `plugin`.
fn wired_service(plugin: Arc<MockPlugin>) -> (Service, Arc<RecordingMetrics>) {
    let metrics = Arc::new(RecordingMetrics::default());
    let hub = hub_with_registry_and_plugin(
        &test_instance_id(),
        VENDOR,
        plugin as Arc<dyn LicenseResolverPluginClient>,
    );
    let svc = service_with(
        hub,
        FakeContractRegistry::with_test_contracts(),
        metrics.clone(),
    );
    (svc, metrics)
}

fn nonconforming_request() -> LicenseCheckRequest {
    request(
        subject(USER_SUBJECT, None, json!({ "category": 42 })),
        resource(
            MODEL_USAGE_RESOURCE,
            Some("gpt-4o"),
            json!({ "model_vendor": "openai", "model_name": "gpt-4o" }),
        ),
    )
}

/// The request crosses to the backend byte-for-byte, tenant scope included —
/// there is no cross-tenant resolution because the resolver never rewrites it.
#[tokio::test]
async fn conforming_request_reaches_the_plugin_unchanged() {
    let plugin = MockPlugin::granting();
    let (svc, _metrics) = wired_service(plugin.clone());
    let sent = conforming_request();

    let decision = svc
        .is_licensed(sent.clone())
        .await
        .expect("conforming check succeeds");

    assert!(decision.granted);
    assert_eq!(
        decision.diagnostics.get("backend").and_then(|v| v.as_str()),
        Some("mock"),
        "the backend's diagnostics must reach the caller"
    );

    let seen = plugin.seen();
    assert_eq!(seen.len(), 1, "the plugin must be called exactly once");
    assert_eq!(seen[0], sent, "the request must be forwarded unchanged");
    assert_eq!(seen[0].context.tenant_id, test_tenant());
}

#[tokio::test]
async fn a_not_granted_answer_is_a_decision_not_an_error() {
    let (svc, metrics) = wired_service(MockPlugin::denying());
    let decision = svc
        .is_licensed(conforming_request())
        .await
        .expect("a denial is still a successful check");
    assert!(!decision.granted);
    assert_eq!(
        metrics.checks(),
        vec![(MODEL_USAGE_RESOURCE.to_owned(), CheckOutcome::NotGranted)]
    );
}

#[tokio::test]
async fn plugin_selection_is_memoized_across_checks() {
    let metrics = Arc::new(RecordingMetrics::default());
    let instance_id = test_instance_id();
    let (hub, registry) = counting_hub_with_registry_only(&instance_id, VENDOR);
    hub.register_scoped::<dyn LicenseResolverPluginClient>(
        ClientScope::gts_id(&instance_id),
        MockPlugin::granting() as Arc<dyn LicenseResolverPluginClient>,
    );
    let svc = service_with(hub, FakeContractRegistry::with_test_contracts(), metrics);

    for _ in 0..2 {
        svc.is_licensed(conforming_request())
            .await
            .expect("conforming check succeeds");
    }
    assert_eq!(
        registry.list_instance_calls(),
        1,
        "discovery must be memoized after the first resolution"
    );
}

/// A non-conforming request is refused on its own terms. If selection ran first,
/// the answer would depend on whether a backend happened to be reachable — and
/// an invalid check must be refused either way.
#[tokio::test]
async fn validation_precedes_plugin_selection() {
    let metrics = Arc::new(RecordingMetrics::default());
    // No types-registry in the hub at all, so selection could only fail.
    let svc = service_with(
        empty_hub(),
        FakeContractRegistry::with_test_contracts(),
        metrics,
    );
    let err = svc
        .is_licensed(nonconforming_request())
        .await
        .expect_err("a non-conforming request must be refused");
    assert!(
        matches!(err, DomainError::ContractViolation { .. }),
        "expected the contract violation, not a selection failure: {err:?}"
    );
}

#[tokio::test]
async fn a_rejected_request_never_reaches_the_plugin() {
    let plugin = MockPlugin::granting();
    let (svc, metrics) = wired_service(plugin.clone());
    let err = svc
        .is_licensed(nonconforming_request())
        .await
        .expect_err("a non-conforming request must be refused");

    assert!(
        matches!(err, DomainError::ContractViolation { .. }),
        "got: {err:?}"
    );
    assert!(
        plugin.seen().is_empty(),
        "a non-conforming request must not be evaluated by any backend"
    );
    assert_eq!(
        metrics.violation_kinds(),
        vec![ViolationKind::SchemaMismatch]
    );
}

/// A backend may reject a conforming request over a constraint its contract does
/// not express. Those violations reach the caller, but they are the backend's
/// classification rather than this gear's validation outcome, so they are not
/// counted as validation failures.
#[tokio::test]
async fn backend_violations_never_become_metric_labels() {
    let (svc, metrics) = wired_service(MockPlugin::failing(LicenseResolverError::InvalidRequest {
        violations: vec![FieldViolation::new(
            "upstream/query",
            "the licensing service rejected the query",
            "BACKEND_SPECIFIC_CODE",
        )],
    }));

    let err = svc
        .is_licensed(conforming_request())
        .await
        .expect_err("a backend rejection is not a decision");

    let DomainError::ContractViolation { violations } = &err else {
        panic!("the backend's violations must reach the caller, got: {err:?}");
    };
    assert_eq!(violations[0].reason, "BACKEND_SPECIFIC_CODE");
    assert!(
        metrics.violation_kinds().is_empty(),
        "an unbounded backend reason must not be recorded: {:?}",
        metrics.violation_kinds()
    );
    assert_eq!(
        metrics.checks(),
        vec![(
            MODEL_USAGE_RESOURCE.to_owned(),
            CheckOutcome::InvalidRequest
        )]
    );
}

#[tokio::test]
async fn no_plugin_instance_fails_closed() {
    let metrics = Arc::new(RecordingMetrics::default());
    let svc = service_with(
        hub_without_plugin_instances(),
        FakeContractRegistry::with_test_contracts(),
        metrics.clone(),
    );
    let err = svc
        .is_licensed(conforming_request())
        .await
        .expect_err("no backend means no answer");
    assert!(
        matches!(err, DomainError::PluginNotFound { .. }),
        "got: {err:?}"
    );
    assert_eq!(
        metrics.checks(),
        vec![(MODEL_USAGE_RESOURCE.to_owned(), CheckOutcome::NoPlugin)]
    );
}

#[tokio::test]
async fn an_advertised_plugin_without_a_client_fails_closed() {
    let metrics = Arc::new(RecordingMetrics::default());
    let (hub, _registry) = counting_hub_with_registry_only(&test_instance_id(), VENDOR);
    let svc = service_with(hub, FakeContractRegistry::with_test_contracts(), metrics);
    let err = svc
        .is_licensed(conforming_request())
        .await
        .expect_err("an unregistered plugin client means no answer");
    assert!(
        matches!(err, DomainError::PluginUnavailable { .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn a_vendor_mismatch_fails_closed() {
    let metrics = Arc::new(RecordingMetrics::default());
    let hub = hub_with_registry_and_plugin(
        &test_instance_id(),
        "some-other-vendor",
        MockPlugin::granting() as Arc<dyn LicenseResolverPluginClient>,
    );
    let svc = service_with(hub, FakeContractRegistry::with_test_contracts(), metrics);
    let err = svc
        .is_licensed(conforming_request())
        .await
        .expect_err("a plugin from another vendor must not be selected");
    assert!(
        matches!(err, DomainError::PluginNotFound { .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn a_failing_backend_fails_closed() {
    let (svc, metrics) = wired_service(MockPlugin::failing(
        LicenseResolverError::ServiceUnavailable("backend timeout".to_owned()),
    ));
    let err = svc
        .is_licensed(conforming_request())
        .await
        .expect_err("an erroring backend means no answer");
    assert!(
        matches!(err, DomainError::PluginUnavailable { ref gts_id, .. } if gts_id == &test_instance_id()),
        "expected PluginUnavailable naming the instance, got: {err:?}"
    );
    assert_eq!(
        metrics.checks(),
        vec![(MODEL_USAGE_RESOURCE.to_owned(), CheckOutcome::Unavailable)]
    );
}

/// The whole fail-closed requirement in one assertion: across every condition
/// that stops the resolver from getting an authoritative answer, not one of them
/// yields a granted decision.
#[tokio::test]
async fn no_failure_path_ever_grants() {
    let instance_id = test_instance_id();
    let granting = || MockPlugin::granting() as Arc<dyn LicenseResolverPluginClient>;

    let cases: Vec<(&str, Service, LicenseCheckRequest)> = vec![
        (
            "no types-registry at all",
            service_with(
                empty_hub(),
                FakeContractRegistry::with_test_contracts(),
                Arc::new(RecordingMetrics::default()),
            ),
            conforming_request(),
        ),
        (
            "no plugin registered",
            service_with(
                hub_without_plugin_instances(),
                FakeContractRegistry::with_test_contracts(),
                Arc::new(RecordingMetrics::default()),
            ),
            conforming_request(),
        ),
        (
            "plugin advertised but client missing",
            service_with(
                counting_hub_with_registry_only(&instance_id, VENDOR).0,
                FakeContractRegistry::with_test_contracts(),
                Arc::new(RecordingMetrics::default()),
            ),
            conforming_request(),
        ),
        (
            "contract registry unreachable",
            service_with(
                hub_with_registry_and_plugin(&instance_id, VENDOR, granting()),
                FakeContractRegistry::failing(FailureKind::Unavailable),
                Arc::new(RecordingMetrics::default()),
            ),
            conforming_request(),
        ),
        (
            "contracts not registered",
            service_with(
                hub_with_registry_and_plugin(&instance_id, VENDOR, granting()),
                FakeContractRegistry::new(),
                Arc::new(RecordingMetrics::default()),
            ),
            conforming_request(),
        ),
        (
            "request does not conform",
            service_with(
                hub_with_registry_and_plugin(&instance_id, VENDOR, granting()),
                FakeContractRegistry::with_test_contracts(),
                Arc::new(RecordingMetrics::default()),
            ),
            nonconforming_request(),
        ),
    ];

    for (label, svc, req) in cases {
        let result = svc.is_licensed(req).await;
        assert!(
            result.is_err(),
            "{label}: a cannot-determine condition must never produce a decision, got: {result:?}"
        );
    }
}

#[tokio::test]
async fn resolver_latency_is_recorded_once_per_check() {
    let (svc, metrics) = wired_service(MockPlugin::granting());
    svc.is_licensed(conforming_request())
        .await
        .expect("conforming check succeeds");
    assert_eq!(metrics.latency_count(), 1);
}

/// A caller-supplied contract type is only a bounded label once it is known to be
/// registered; before that it would let any caller grow the metric's label space.
#[tokio::test]
async fn an_unvalidated_contract_type_never_becomes_a_metric_label() {
    let metrics = Arc::new(RecordingMetrics::default());
    let svc = service_with(
        hub_with_registry_and_plugin(
            &test_instance_id(),
            VENDOR,
            MockPlugin::granting() as Arc<dyn LicenseResolverPluginClient>,
        ),
        FakeContractRegistry::new(),
        metrics.clone(),
    );
    svc.is_licensed(conforming_request())
        .await
        .expect_err("unregistered contracts must be refused");

    assert_eq!(
        metrics.latency_count(),
        1,
        "the boundary latency is recorded even for a refused check"
    );
    assert_eq!(
        metrics.checks(),
        vec![(
            UNVALIDATED_CONTRACT.to_owned(),
            CheckOutcome::InvalidRequest
        )]
    );
}

#[tokio::test]
async fn a_mapped_principal_reaches_the_plugin_as_a_subject() {
    let plugin = MockPlugin::granting();
    let (svc, metrics) = wired_service(plugin.clone());
    let principal_id = Uuid::from_u128(7);

    let decision = svc
        .is_principal_licensed(
            Some(OPERATOR_PRINCIPAL),
            principal_id,
            model_usage(),
            test_context(),
        )
        .await
        .expect("mapped principal check succeeds");

    assert!(decision.granted);
    let seen = plugin.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].subject,
        subject(SEAT_SUBJECT, Some(&principal_id.to_string()), json!({}))
    );
    assert_eq!(seen[0].resource, model_usage());
    assert_eq!(seen[0].context.tenant_id, test_tenant());
    assert_eq!(metrics.latency_count(), 1);
}

#[tokio::test]
async fn an_unmapped_principal_is_not_granted_without_the_plugin() {
    let plugin = MockPlugin::granting();
    let (svc, metrics) = wired_service(plugin.clone());

    let decision = svc
        .is_principal_licensed(
            Some("app"),
            Uuid::from_u128(7),
            model_usage(),
            test_context(),
        )
        .await
        .expect("an unmapped principal is a decision");

    assert!(!decision.granted);
    assert_eq!(
        decision.diagnostics.get("reason").and_then(|v| v.as_str()),
        Some("principal_not_mapped")
    );
    assert!(plugin.seen().is_empty());
    assert_eq!(
        metrics.checks(),
        vec![(
            UNVALIDATED_CONTRACT.to_owned(),
            CheckOutcome::PrincipalNotMapped
        )]
    );
    assert_eq!(metrics.latency_count(), 1);
}

#[tokio::test]
async fn an_anonymous_principal_is_not_mapped() {
    let nil_tenant = LicenseCheckContext::builder()
        .tenant_id(Uuid::nil())
        .build()
        .expect("tenant is set");
    for (principal_id, context) in [
        (Uuid::nil(), test_context()),
        (Uuid::from_u128(7), nil_tenant),
    ] {
        let plugin = MockPlugin::granting();
        let (svc, metrics) = wired_service(plugin.clone());

        let decision = svc
            .is_principal_licensed(
                Some(OPERATOR_PRINCIPAL),
                principal_id,
                model_usage(),
                context,
            )
            .await
            .expect("an anonymous principal is a decision");

        assert!(!decision.granted);
        assert!(plugin.seen().is_empty());
        assert_eq!(
            metrics.checks(),
            vec![(
                UNVALIDATED_CONTRACT.to_owned(),
                CheckOutcome::PrincipalNotMapped
            )]
        );
    }
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_backend_rejection_of_a_mapped_subject_is_not_logged_as_misconfiguration() {
    let (svc, _metrics) =
        wired_service(MockPlugin::failing(LicenseResolverError::InvalidRequest {
            violations: vec![FieldViolation::new(
                "subject/id",
                "seat is held elsewhere",
                "SEAT_HELD",
            )],
        }));

    let err = svc
        .is_principal_licensed(
            Some(OPERATOR_PRINCIPAL),
            Uuid::from_u128(7),
            model_usage(),
            test_context(),
        )
        .await
        .expect_err("the backend rejects the subject");

    assert!(
        matches!(err, DomainError::ContractViolation { .. }),
        "got: {err:?}"
    );
    assert!(!logs_contain("principal_subject_types maps"));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_mapped_type_that_requires_metadata_is_an_invalid_request() {
    let plugin = MockPlugin::granting();
    let (svc, _metrics) = wired_service(plugin.clone());

    let err = svc
        .is_principal_licensed(
            Some(METADATA_PRINCIPAL),
            Uuid::from_u128(7),
            model_usage(),
            test_context(),
        )
        .await
        .expect_err("empty metadata does not conform");

    assert!(
        matches!(err, DomainError::ContractViolation { .. }),
        "got: {err:?}"
    );
    assert!(plugin.seen().is_empty());
    assert!(logs_contain(
        "principal_subject_types maps to a Subject contract type"
    ));
}

#[tokio::test]
async fn an_unresolvable_principal_mapping_fails_closed() {
    let metrics = Arc::new(RecordingMetrics::default());
    let svc = service_with(
        hub_with_registry_and_plugin(
            &test_instance_id(),
            VENDOR,
            MockPlugin::granting() as Arc<dyn LicenseResolverPluginClient>,
        ),
        FakeContractRegistry::failing(FailureKind::Unavailable),
        metrics.clone(),
    );

    let err = svc
        .is_principal_licensed(
            Some(OPERATOR_PRINCIPAL),
            Uuid::from_u128(7),
            model_usage(),
            test_context(),
        )
        .await
        .expect_err("the mapping cannot be determined");

    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "got: {err:?}"
    );
    assert_eq!(metrics.checks().len(), 1);
    assert_eq!(metrics.latency_count(), 1);
}
