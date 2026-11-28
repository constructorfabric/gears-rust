//! Unit tests for the classification of registry failures.
//!
//! The three-way split is the whole point of the adapter: a caller's typo, an
//! unregistered contract, and an outage must not be reported as each other.

use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

use super::*;
use crate::domain::test_support::{USER_SUBJECT, user_subject_schema};

/// A registry holding the user subject contract and its base.
fn registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    for document in [
        crate::test_contracts::subject_base_schema(),
        user_subject_schema(),
    ] {
        registry.seed(
            document.type_id.as_ref(),
            document.content.expect("content"),
        );
    }
    registry
}

fn adapter(registry: &Arc<MockTypesRegistry>) -> GtsContractRegistry {
    let hub = Arc::new(ClientHub::default());
    registry.install(&hub);
    GtsContractRegistry::new(hub)
}

#[tokio::test]
async fn a_registered_contract_resolves_with_its_materialized_artifacts() {
    let contract = adapter(&registry())
        .contract_schema(USER_SUBJECT)
        .await
        .expect("a registered contract resolves");
    assert_eq!(contract.type_id.as_ref(), USER_SUBJECT);
    assert!(contract.content.is_some(), "authored content");
    assert!(contract.resolved_schema.is_some(), "the chain resolved");
    assert!(contract.effective_traits.is_some(), "the merged traits");
}

#[tokio::test]
async fn an_absent_or_deleted_contract_is_unregistered() {
    let empty = Arc::new(MockTypesRegistry::new());
    let err = adapter(&empty)
        .contract_schema(USER_SUBJECT)
        .await
        .expect_err("nothing is registered");
    assert!(
        matches!(err, ContractRegistryError::Unregistered),
        "expected Unregistered, got: {err:?}"
    );

    let registry = registry();
    let content = registry.content(USER_SUBJECT).expect("seeded");
    registry.seed_entity(Seed::new(USER_SUBJECT, content).deleted());
    let err = adapter(&registry)
        .contract_schema(USER_SUBJECT)
        .await
        .expect_err("a deleted contract is no longer registered");
    assert!(
        matches!(err, ContractRegistryError::Unregistered),
        "expected Unregistered, got: {err:?}"
    );
}

/// A malformed id is the caller's mistake. Folding it into the outage arm would
/// tell an operator the registry is down because a caller sent a typo.
#[tokio::test]
async fn a_malformed_type_id_is_the_callers_fault_not_an_outage() {
    let adapter = adapter(&Arc::new(MockTypesRegistry::new()));

    for bad in ["", "not-a-gts-id", "gts.cf.core.lic.subj.v1"] {
        let err = match adapter.contract_schema(bad).await {
            Err(err) => err,
            Ok(schema) => panic!("'{bad}' must not resolve, got {}", schema.type_id),
        };
        assert!(
            matches!(err, ContractRegistryError::MalformedTypeId(_)),
            "'{bad}' must be reported as malformed, got: {err:?}"
        );
    }
}

#[tokio::test]
async fn any_other_registry_failure_is_an_outage() {
    let registry = registry();
    registry.inject(
        Fault::on(Call::BatchGet).fail(CanonicalError::internal("connection reset").create()),
    );
    let err = adapter(&registry)
        .contract_schema(USER_SUBJECT)
        .await
        .expect_err("an internal registry failure must not resolve");
    assert!(
        matches!(err, ContractRegistryError::Unavailable(_)),
        "expected Unavailable, got: {err:?}"
    );
}

/// The client is looked up per call, so a hub that has no types-registry yet is
/// an outage rather than a panic or a bad request.
#[tokio::test]
async fn a_hub_without_a_registry_is_an_outage() {
    let adapter = GtsContractRegistry::new(Arc::new(ClientHub::default()));
    let err = adapter
        .contract_schema(USER_SUBJECT)
        .await
        .expect_err("no client means no answer");
    assert!(
        matches!(err, ContractRegistryError::Unavailable(_)),
        "expected Unavailable, got: {err:?}"
    );
}

/// The platform registry over `inner`, answering every read without the
/// resolved schema: a registry this process disagrees with.
struct WithoutResolvedSchema(Arc<MockTypesRegistry>);

#[async_trait]
impl PlatformTypesRegistryApi for WithoutResolvedSchema {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        let mut answer =
            PlatformTypesRegistryApi::batch_get_entities(self.0.as_ref(), ctx, request).await?;
        for lookup in answer.0.values_mut() {
            if let types_registry_sdk::EntityLookup::Found { entity, .. } = lookup {
                entity.resolved_schema = None;
            }
        }
        Ok(answer)
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        PlatformTypesRegistryApi::list_entities(self.0.as_ref(), ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: types_registry_sdk::IdempotencyKey,
        request: types_registry_sdk::RegisterEntitiesRequest,
    ) -> Result<types_registry_sdk::RegistrationOperation, CanonicalError> {
        self.0.register_entities(ctx, key, request).await
    }

    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: types_registry_sdk::IdempotencyKey,
        request: types_registry_sdk::DeleteEntitiesRequest,
    ) -> Result<types_registry_sdk::DeletionOperation, CanonicalError> {
        self.0.delete_entities(ctx, key, request).await
    }

    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: uuid::Uuid,
    ) -> Result<types_registry_sdk::Operation, CanonicalError> {
        self.0.get_operation(ctx, operation_id).await
    }

    async fn batch_resolve_identities(
        &self,
        ctx: &PlatformSecurityContext,
        keys: Vec<types_registry_sdk::EntityKey>,
    ) -> Result<
        std::collections::HashMap<
            types_registry_sdk::EntityKey,
            Option<types_registry_sdk::EntityIdentity>,
        >,
        CanonicalError,
    > {
        PlatformTypesRegistryApi::batch_resolve_identities(self.0.as_ref(), ctx, keys).await
    }
}

/// A registry answer without a selected artifact is the registry's answer,
/// not an outage: the adapter returns it, and validation reports the contract
/// unusable (`a_missing_materialized_artifact_is_catalog_drift`).
#[tokio::test]
async fn a_contract_missing_an_artifact_is_returned_not_an_outage() {
    let hub = Arc::new(ClientHub::default());
    hub.register::<dyn PlatformTypesRegistryApi>(Arc::new(WithoutResolvedSchema(registry())));
    let contract = GtsContractRegistry::new(hub)
        .contract_schema(USER_SUBJECT)
        .await
        .expect("the registry answered");
    assert!(contract.resolved_schema.is_none());
    assert!(contract.content.is_some());
}
