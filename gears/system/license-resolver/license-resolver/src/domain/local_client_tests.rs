//! Unit tests for the boundary the local client draws: domain errors become
//! canonical errors, decisions pass through.

use license_resolver_sdk::{LicenseResolverPluginClient, field};
use serde_json::json;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::Problem;

use super::*;
use crate::domain::ports::{LicenseMetrics, NoopMetrics};
use crate::domain::test_support::{
    FakeContractRegistry, MODEL_USAGE_RESOURCE, MockPlugin, USER_SUBJECT, conforming_request,
    empty_hub, hub_with_registry_and_plugin, request, resource, subject, test_instance_id,
};
use crate::domain::validation::ContractValidator;

const VENDOR: &str = "constructorfabric";

fn client(hub: Arc<ClientHub>, registry: FakeContractRegistry) -> LicenseResolverLocalClient {
    let svc = Arc::new(Service::new(
        hub,
        VENDOR.to_owned(),
        ContractValidator::new(Arc::new(registry)),
        Arc::new(NoopMetrics) as Arc<dyn LicenseMetrics>,
    ));
    LicenseResolverLocalClient::new(svc)
}

fn granting_plugin() -> Arc<dyn LicenseResolverPluginClient> {
    MockPlugin::granting() as Arc<dyn LicenseResolverPluginClient>
}

#[tokio::test]
async fn a_decision_passes_through_the_boundary() {
    let client = client(
        hub_with_registry_and_plugin(&test_instance_id(), VENDOR, granting_plugin()),
        FakeContractRegistry::with_test_contracts(),
    );
    let decision = client
        .is_licensed(
            PlatformSecurityContext::outbound_marker(),
            conforming_request(),
        )
        .await
        .expect("conforming check succeeds");
    assert!(decision.granted);
}

#[tokio::test]
async fn a_contract_violation_becomes_invalid_request() {
    let client = client(
        hub_with_registry_and_plugin(&test_instance_id(), VENDOR, granting_plugin()),
        FakeContractRegistry::with_test_contracts(),
    );
    let req = request(
        subject(USER_SUBJECT, None, json!({ "category": 42 })),
        resource(
            MODEL_USAGE_RESOURCE,
            None,
            json!({ "model_vendor": "openai", "model_name": "gpt-4o" }),
        ),
    );
    let err = client
        .is_licensed(PlatformSecurityContext::outbound_marker(), req)
        .await
        .expect_err("a non-conforming request must be refused");
    assert!(
        matches!(err, CanonicalError::InvalidArgument { .. }),
        "got: {err:?}"
    );
    let wire = serde_json::to_value(Problem::from_error(&err).expect("problem renders")).unwrap();
    assert_eq!(
        wire.pointer("/context/field_violations/0/reason")
            .and_then(|v| v.as_str()),
        Some(field::SCHEMA_MISMATCH)
    );
    assert!(wire.pointer("/context/field_violations/1").is_none());
}

/// Hub without a types-registry → `TypesRegistryUnavailable` → the caller sees a
/// retryable unavailable, never a grant.
#[tokio::test]
async fn an_unreachable_registry_becomes_service_unavailable() {
    let client = client(empty_hub(), FakeContractRegistry::with_test_contracts());
    let err = client
        .is_licensed(
            PlatformSecurityContext::outbound_marker(),
            conforming_request(),
        )
        .await
        .expect_err("no registry means no answer");
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got: {err:?}"
    );
}
