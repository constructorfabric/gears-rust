#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, ProtocolFault, Seed};
use types_registry_sdk::{PlatformTypesRegistryApi, TypesRegistryApi};

use super::RegistryTypeCatalog;
use crate::domain::ports::{PortError, TypeCatalogPort, TypeCatalogScope};
use crate::test_support::{WIDGET, ctx};

#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.core.types_registry.entity.v1~"))]
struct RegistryReadResource;

const UNKNOWN: &str = "gts.cf.core.policy_engine.unknown.v1~";

fn registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(
        WIDGET,
        serde_json::json!({
            "$id": format!("gts://{WIDGET}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
        }),
    );
    registry
}

fn catalog(registry: &Arc<MockTypesRegistry>) -> RegistryTypeCatalog {
    RegistryTypeCatalog::new(
        Arc::clone(registry) as Arc<dyn TypesRegistryApi>,
        Arc::clone(registry) as Arc<dyn PlatformTypesRegistryApi>,
        Duration::from_secs(1),
    )
}

#[tokio::test]
async fn known_types_reports_only_registered_type_identifiers() {
    let registry = registry();
    let known = catalog(&registry)
        .known_types(
            TypeCatalogScope::Platform,
            &[
                WIDGET.to_owned(),
                UNKNOWN.to_owned(),
                "not-an-id".to_owned(),
                WIDGET.to_owned(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(known, std::collections::HashSet::from([WIDGET.to_owned()]));
    assert_eq!(registry.platform_calls(Call::BatchGet), 1);
}

async fn assert_deleted_types_are_not_known(scope: TypeCatalogScope<'_>) {
    let registry = registry();
    registry.seed_entity(
        Seed::new(
            UNKNOWN,
            serde_json::json!({
                "$id": format!("gts://{UNKNOWN}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
            }),
        )
        .deleted(),
    );

    let known = catalog(&registry)
        .known_types(scope, &[WIDGET.to_owned(), UNKNOWN.to_owned()])
        .await
        .unwrap();

    assert_eq!(known, std::collections::HashSet::from([WIDGET.to_owned()]));
}

#[tokio::test]
async fn platform_validation_excludes_deleted_resource_types() {
    assert_deleted_types_are_not_known(TypeCatalogScope::Platform).await;
}

#[tokio::test]
async fn caller_validation_excludes_deleted_resource_types() {
    let caller = ctx(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    assert_deleted_types_are_not_known(TypeCatalogScope::Caller(&caller)).await;
}

#[tokio::test]
async fn an_empty_request_does_not_call_the_registry() {
    let registry = Arc::new(MockTypesRegistry::strict());
    assert!(
        catalog(&registry)
            .known_types(TypeCatalogScope::Platform, &[])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(registry.calls(Call::BatchGet), 0);
}

#[tokio::test]
async fn caller_reads_keep_the_request_identity_and_never_use_the_platform() {
    let registry = registry();
    let subject = uuid::Uuid::new_v4();
    let tenant = uuid::Uuid::new_v4();
    let caller = ctx(subject, tenant);
    let known = catalog(&registry)
        .known_types(TypeCatalogScope::Caller(&caller), &[WIDGET.to_owned()])
        .await
        .unwrap();
    assert!(known.contains(WIDGET));
    let contexts = registry.caller_contexts(Call::BatchGet);
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].subject_id(), subject);
    assert_eq!(contexts[0].subject_tenant_id(), tenant);
    assert_eq!(registry.platform_calls(Call::BatchGet), 0);
}

#[tokio::test]
async fn caller_refusals_are_not_unknown_types_or_a_platform_fallback() {
    for (error, expected) in [
        (
            CanonicalError::unauthenticated()
                .with_reason("CALLER_AUTH")
                .create(),
            PortError::Unauthenticated,
        ),
        (
            RegistryReadResource::permission_denied()
                .with_reason("CALLER_DENIED")
                .create(),
            PortError::PermissionDenied,
        ),
    ] {
        let registry = registry();
        registry.inject(Fault::on(Call::BatchGet).fail(error));
        let caller = ctx(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let error = catalog(&registry)
            .known_types(TypeCatalogScope::Caller(&caller), &[WIDGET.to_owned()])
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert_eq!(registry.caller_contexts(Call::BatchGet).len(), 1);
        assert_eq!(registry.platform_calls(Call::BatchGet), 0);
    }
}

#[tokio::test]
async fn a_platform_credential_refusal_is_a_dependency_failure() {
    let registry = registry();
    registry.inject(
        Fault::on(Call::BatchGet).fail(
            RegistryReadResource::permission_denied()
                .with_reason("CALLER_DENIED")
                .create(),
        ),
    );
    let error = catalog(&registry)
        .known_types(TypeCatalogScope::Platform, &[WIDGET.to_owned()])
        .await
        .unwrap_err();
    assert!(matches!(error, PortError::Unavailable(_)));
}

#[tokio::test(start_paused = true)]
async fn a_stalled_batch_fails_at_the_configured_budget() {
    let registry = registry();
    registry.inject(Fault::on(Call::BatchGet).delay(Duration::from_secs(60)));
    let error = catalog(&registry)
        .known_types(TypeCatalogScope::Platform, &[WIDGET.to_owned()])
        .await
        .unwrap_err();
    assert_eq!(error, PortError::Timeout);
}

#[tokio::test]
async fn an_unusable_batch_never_becomes_a_partial_known_set() {
    for malformed in [false, true] {
        let registry = registry();
        if malformed {
            registry.protocol_fault(ProtocolFault::DropAnswers);
        } else {
            registry.inject(
                Fault::on(Call::BatchGet).fail(CanonicalError::internal("outage").create()),
            );
        }
        let error = catalog(&registry)
            .known_types(
                TypeCatalogScope::Platform,
                &[WIDGET.to_owned(), UNKNOWN.to_owned()],
            )
            .await
            .unwrap_err();
        assert!(matches!(error, PortError::Unavailable(_)), "{error:?}");
    }
}
