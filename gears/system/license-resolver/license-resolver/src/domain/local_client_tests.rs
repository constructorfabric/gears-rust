//! Unit tests for the boundary the local client draws: domain errors become
//! canonical errors, decisions pass through.

use license_resolver_sdk::{
    LicenseResolverPluginClient, PrincipalSubject, Resource, Subject, SubjectInput, field,
};
use serde_json::json;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::Problem;

use super::*;
use crate::domain::ports::{LicenseMetrics, NoopMetrics};
use crate::domain::principal::PrincipalMapper;
use crate::domain::test_support::{
    FakeContractRegistry, MODEL_USAGE_RESOURCE, MockPlugin, SEAT_SUBJECT, USER_SUBJECT,
    conforming_request, empty_hub, hub_with_registry_and_plugin, model_usage, principal_mapping,
    resource, subject, test_context, test_instance_id,
};
use crate::domain::validation::ContractValidator;
use uuid::Uuid;

const VENDOR: &str = "constructorfabric";

fn client(hub: Arc<ClientHub>, registry: FakeContractRegistry) -> LicenseResolverLocalClient {
    let registry: Arc<dyn crate::domain::ContractRegistry> = Arc::new(registry);
    let svc = Arc::new(Service::new(
        hub,
        VENDOR.to_owned(),
        ContractValidator::new(registry.clone()),
        PrincipalMapper::new(principal_mapping(&[("user", SEAT_SUBJECT)]), registry),
        Arc::new(NoopMetrics) as Arc<dyn LicenseMetrics>,
    ));
    LicenseResolverLocalClient::new(svc)
}

fn public(request: LicenseCheckRequest) -> IsLicensedRequest {
    IsLicensedRequest::new(request.subject, request.resource, request.context)
}

fn check(subject: impl Into<SubjectInput>, resource: Resource) -> IsLicensedRequest {
    IsLicensedRequest::new(subject, resource, test_context())
}

fn caller(subject_type: Option<&str>) -> SecurityContext {
    let builder = SecurityContext::builder()
        .subject_id(Uuid::from_u128(7))
        .subject_tenant_id(Uuid::from_u128(42));
    match subject_type {
        Some(subject_type) => builder.subject_type(subject_type),
        None => builder,
    }
    .build()
    .expect("valid security context")
}

fn seat_of(id: Uuid) -> Subject {
    subject(SEAT_SUBJECT, Some(&id.to_string()), json!({}))
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
            public(conforming_request()),
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
    let req = check(
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
            public(conforming_request()),
        )
        .await
        .expect_err("no registry means no answer");
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got: {err:?}"
    );
}

#[tokio::test]
async fn a_principal_subject_is_checked_as_its_mapped_subject() {
    let plugin = MockPlugin::granting();
    let client = client(
        hub_with_registry_and_plugin(
            &test_instance_id(),
            VENDOR,
            plugin.clone() as Arc<dyn LicenseResolverPluginClient>,
        ),
        FakeContractRegistry::with_test_contracts(),
    );
    let id = Uuid::from_u128(7);

    let decision = client
        .is_licensed(
            PlatformSecurityContext::outbound_marker(),
            check(PrincipalSubject::new("user", id), model_usage()),
        )
        .await
        .expect("mapped principal check succeeds");

    assert!(decision.granted);
    assert_eq!(plugin.seen()[0].subject, seat_of(id));
}

#[tokio::test]
async fn the_self_check_takes_subject_and_tenant_from_the_context() {
    let plugin = MockPlugin::granting();
    let client = client(
        hub_with_registry_and_plugin(
            &test_instance_id(),
            VENDOR,
            plugin.clone() as Arc<dyn LicenseResolverPluginClient>,
        ),
        FakeContractRegistry::with_test_contracts(),
    );

    let decision = client
        .is_self_licensed(
            caller(Some("user")),
            SelfLicenseCheckRequest::new(model_usage()),
        )
        .await
        .expect("self check succeeds");

    assert!(decision.granted);
    let seen = plugin.seen();
    assert_eq!(seen[0].subject, seat_of(Uuid::from_u128(7)));
    assert_eq!(seen[0].context.tenant_id, Uuid::from_u128(42));
}

#[tokio::test]
async fn a_caller_without_a_principal_type_is_not_granted() {
    let plugin = MockPlugin::granting();
    let client = client(
        hub_with_registry_and_plugin(
            &test_instance_id(),
            VENDOR,
            plugin.clone() as Arc<dyn LicenseResolverPluginClient>,
        ),
        FakeContractRegistry::with_test_contracts(),
    );

    let decision = client
        .is_self_licensed(caller(None), SelfLicenseCheckRequest::new(model_usage()))
        .await
        .expect("an unmapped caller is a decision");

    assert!(!decision.granted);
    assert!(plugin.seen().is_empty());
}
