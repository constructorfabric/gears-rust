//! Transitional persistent-read overlay for Account Management's configured root type.
//!
//! Until Types Registry T24 removes the process-local catalogue, AM still needs
//! the legacy client for deployment-owned schemas and plugin instances. This
//! adapter routes the configured root type and its derivation chain exclusively
//! to authoritative storage while delegating every unrelated operation.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gts::{GtsId, GtsIdPattern};
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{
    EntityKind, GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, LifecycleStatus,
    RegisterResult, TypeSchemaQuery, TypesRegistryClient, TypesRegistryEntities,
};
use uuid::Uuid;

use crate::domain::root_type::RootTypeConfig;

use super::root_type::validate_persisted_root;

/// Legacy-client-compatible view whose configured root reads are authoritative.
// TODO(#4627): remove this overlay when all Types Registry reads use the
// persistent SDK directly.
pub struct RootTypeAwareRegistryClient {
    legacy: Arc<dyn TypesRegistryClient>,
    persistent: Arc<dyn TypesRegistryEntities>,
    root_type: RootTypeConfig,
    root_uuid: Uuid,
}

impl RootTypeAwareRegistryClient {
    /// Construct the transitional overlay after root reconciliation succeeds.
    ///
    /// # Errors
    /// Returns a diagnostic if `root_type` is not a canonical concrete AM type.
    pub fn new(
        legacy: Arc<dyn TypesRegistryClient>,
        persistent: Arc<dyn TypesRegistryEntities>,
        root_type: RootTypeConfig,
    ) -> anyhow::Result<Self> {
        let root_id = root_type.validated_id().map_err(anyhow::Error::msg)?;
        let root_uuid = GtsId::try_new(root_id)?.to_uuid();
        Ok(Self {
            legacy,
            persistent,
            root_type,
            root_uuid,
        })
    }

    fn is_root_id(&self, type_id: &str) -> bool {
        type_id == self.root_type.gts_id.as_ref()
    }

    fn internal(detail: impl Into<String>) -> CanonicalError {
        CanonicalError::internal(detail).create()
    }

    async fn authoritative_root(&self) -> Result<GtsTypeSchema, CanonicalError> {
        let root_id = self.root_type.gts_id.as_ref();
        let parsed = GtsId::try_new(root_id).map_err(|error| {
            Self::internal(format!(
                "configured root type {root_id} became invalid after startup validation: {error}"
            ))
        })?;
        let chain = parsed.chain_ids();
        let mut parent: Option<Arc<GtsTypeSchema>> = None;

        for chain_id in chain {
            let snapshot = self
                .persistent
                .get_entity(&chain_id)
                .await?
                .ok_or_else(|| {
                    Self::internal(format!(
                        "authoritative root-type chain entity {chain_id} is absent"
                    ))
                })?;
            let expected_uuid = GtsId::try_new(&chain_id)
                .map_err(|error| {
                    Self::internal(format!(
                        "authoritative root-type chain id {chain_id} is invalid: {error}"
                    ))
                })?
                .to_uuid();
            if snapshot.gts_id != chain_id
                || snapshot.gts_uuid != expected_uuid
                || snapshot.kind != EntityKind::TypeSchema
                || snapshot.lifecycle_status != LifecycleStatus::Active
            {
                return Err(Self::internal(format!(
                    "authoritative root-type chain entity {chain_id} is not an active Type Schema"
                )));
            }
            if chain_id == root_id {
                validate_persisted_root(&snapshot, &self.root_type)
                    .map_err(|error| Self::internal(error.to_string()))?;
            }
            let content = snapshot.content.ok_or_else(|| {
                Self::internal(format!(
                    "authoritative root-type chain entity {chain_id} has no authored document"
                ))
            })?;
            let description = content
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let schema =
                GtsTypeSchema::try_new(GtsTypeId::new(&chain_id), content, description, parent)?;
            parent = Some(Arc::new(schema));
        }

        parent
            .map(|schema| schema.as_ref().clone())
            .ok_or_else(|| Self::internal("configured root type has an empty GTS chain"))
    }

    fn query_includes_root(&self, query: &TypeSchemaQuery) -> Result<bool, CanonicalError> {
        let Some(pattern) = query.pattern.as_deref() else {
            return Ok(true);
        };
        let pattern = GtsIdPattern::try_new(pattern).map_err(|error| {
            Self::internal(format!(
                "legacy registry accepted an invalid type-schema pattern `{pattern}`: {error}"
            ))
        })?;
        let root = GtsId::try_new(self.root_type.gts_id.as_ref()).map_err(|error| {
            Self::internal(format!("configured root type became invalid: {error}"))
        })?;
        Ok(root.matches_pattern(&pattern))
    }
}

#[async_trait]
impl TypesRegistryClient for RootTypeAwareRegistryClient {
    async fn register(
        &self,
        entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.legacy.register(entities).await
    }

    async fn register_type_schemas(
        &self,
        type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.legacy.register_type_schemas(type_schemas).await
    }

    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        if self.is_root_id(type_id) {
            self.authoritative_root().await
        } else {
            self.legacy.get_type_schema(type_id).await
        }
    }

    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        if type_uuid == self.root_uuid {
            self.authoritative_root().await
        } else {
            self.legacy.get_type_schema_by_uuid(type_uuid).await
        }
    }

    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        let root_requested = type_ids.iter().any(|type_id| self.is_root_id(type_id));
        let delegated = type_ids
            .into_iter()
            .filter(|type_id| !self.is_root_id(type_id))
            .collect();
        let mut results = self.legacy.get_type_schemas(delegated).await;
        if root_requested {
            results.insert(
                self.root_type.gts_id.to_string(),
                self.authoritative_root().await,
            );
        }
        results
    }

    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        let root_requested = type_uuids.contains(&self.root_uuid);
        let delegated = type_uuids
            .into_iter()
            .filter(|type_uuid| *type_uuid != self.root_uuid)
            .collect();
        let mut results = self.legacy.get_type_schemas_by_uuid(delegated).await;
        if root_requested {
            results.insert(self.root_uuid, self.authoritative_root().await);
        }
        results
    }

    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        let mut results = self.legacy.list_type_schemas(query.clone()).await?;
        let include_root = self.query_includes_root(&query)?;
        results.retain(|schema| !self.is_root_id(schema.type_id.as_ref()));
        if include_root {
            results.push(self.authoritative_root().await?);
        }
        results.sort_by(|left, right| left.type_id.as_ref().cmp(right.type_id.as_ref()));
        Ok(results)
    }

    async fn register_instances(
        &self,
        instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.legacy.register_instances(instances).await
    }

    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        self.legacy.get_instance(id).await
    }

    async fn get_instance_by_uuid(&self, uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        self.legacy.get_instance_by_uuid(uuid).await
    }

    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        self.legacy.get_instances(ids).await
    }

    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        self.legacy.get_instances_by_uuid(uuids).await
    }

    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        self.legacy.list_instances(query).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use account_management_sdk::gts::TenantTypeEnvelopeV1;
    use gts::GtsSchema;
    use serde_json::json;
    use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_type_schema};
    use types_registry_sdk::{EntitySnapshot, RegisterEntities, RegistrationOperation};

    use crate::domain::root_type::TENANT_TYPE_BASE;

    use super::*;
    use crate::infra::types_registry::root_type::desired_root_schema;

    const ROOT: &str = "gts.cf.core.am.tenant_type.v1~cf.core.am.platform.v1~";
    const OTHER: &str = "gts.example.catalog.core.type.v1~";

    struct PersistentFake {
        entities: HashMap<String, EntitySnapshot>,
        fail_reads: bool,
    }

    #[async_trait]
    impl TypesRegistryEntities for PersistentFake {
        async fn get_entity(&self, gts_id: &str) -> Result<Option<EntitySnapshot>, CanonicalError> {
            if self.fail_reads {
                return Err(CanonicalError::service_unavailable()
                    .with_detail("persistent registry unavailable")
                    .create());
            }
            Ok(self.entities.get(gts_id).cloned())
        }

        async fn compare_and_swap_owning_gear(
            &self,
            _gts_id: &str,
            _expected_resource_version: i64,
            _expected_owning_gear: Option<String>,
            _owning_gear: String,
        ) -> Result<bool, CanonicalError> {
            Err(CanonicalError::internal("unexpected ownership CAS").create())
        }

        async fn register_and_await(
            &self,
            _idempotency_key: String,
            _request: RegisterEntities,
            _wait_timeout: Duration,
        ) -> Result<RegistrationOperation, CanonicalError> {
            Err(CanonicalError::internal("unexpected registration").create())
        }
    }

    fn root_config() -> RootTypeConfig {
        RootTypeConfig {
            gts_id: GtsTypeId::new(ROOT),
            idp_provisioning: false,
        }
    }

    fn persistent_root(fail_reads: bool) -> Arc<dyn TypesRegistryEntities> {
        let cfg = root_config();
        let base_content = TenantTypeEnvelopeV1::<()>::gts_schema_with_refs();
        let base = GtsTypeSchema::try_new(
            GtsTypeId::new(TENANT_TYPE_BASE),
            base_content.clone(),
            None,
            None,
        )
        .expect("base schema");
        let root_content = desired_root_schema(&cfg).expect("root schema");
        let root = GtsTypeSchema::try_new(
            GtsTypeId::new(ROOT),
            root_content.clone(),
            None,
            Some(Arc::new(base.clone())),
        )
        .expect("root schema projection");
        let snapshots = [
            EntitySnapshot {
                gts_id: TENANT_TYPE_BASE.to_owned(),
                gts_uuid: base.type_uuid,
                kind: EntityKind::TypeSchema,
                lifecycle_status: LifecycleStatus::Active,
                resource_version: 1,
                owning_gear: Some("account-management".to_owned()),
                content: Some(base_content),
                resolved_schema: Some(base.effective_schema()),
                effective_traits: Some(base.effective_traits()),
                effective_traits_schema: None,
            },
            EntitySnapshot {
                gts_id: ROOT.to_owned(),
                gts_uuid: root.type_uuid,
                kind: EntityKind::TypeSchema,
                lifecycle_status: LifecycleStatus::Active,
                resource_version: 1,
                owning_gear: Some("account-management".to_owned()),
                content: Some(root_content),
                resolved_schema: Some(root.effective_schema()),
                effective_traits: Some(root.effective_traits()),
                effective_traits_schema: None,
            },
        ]
        .into_iter()
        .map(|snapshot| (snapshot.gts_id.clone(), snapshot))
        .collect();
        Arc::new(PersistentFake {
            entities: snapshots,
            fail_reads,
        })
    }

    fn client(
        legacy: Arc<dyn TypesRegistryClient>,
        persistent: Arc<dyn TypesRegistryEntities>,
    ) -> RootTypeAwareRegistryClient {
        RootTypeAwareRegistryClient::new(legacy, persistent, root_config()).expect("valid overlay")
    }

    #[tokio::test]
    async fn exact_root_reads_work_without_a_legacy_seed() {
        let registry = client(
            Arc::new(MockTypesRegistryClient::new()),
            persistent_root(false),
        );

        let by_id = registry.get_type_schema(ROOT).await.expect("root by id");
        let by_uuid = registry
            .get_type_schema_by_uuid(by_id.type_uuid)
            .await
            .expect("root by UUID");
        let by_uuid_batch = registry
            .get_type_schemas_by_uuid(vec![by_id.type_uuid])
            .await;

        assert_eq!(by_id.type_id.as_ref(), ROOT);
        assert_eq!(by_uuid, by_id);
        assert_eq!(
            by_uuid_batch[&by_id.type_uuid]
                .as_ref()
                .expect("root in UUID batch"),
            &by_id
        );
        assert_eq!(
            by_id.parent.as_ref().map(|parent| parent.type_id.as_ref()),
            Some(TENANT_TYPE_BASE)
        );
        assert_eq!(
            by_id.effective_traits(),
            json!({"allowed_parent_types": [], "idp_provisioning": false})
        );
    }

    #[tokio::test]
    async fn mixed_batch_routes_only_the_root_to_persistence() {
        let legacy =
            MockTypesRegistryClient::new().with_type_schemas([make_test_type_schema(OTHER)]);
        let registry = client(Arc::new(legacy), persistent_root(false));

        let results = registry
            .get_type_schemas(vec![ROOT.to_owned(), OTHER.to_owned()])
            .await;

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[ROOT]
                .as_ref()
                .expect("persistent root")
                .type_id
                .as_ref(),
            ROOT
        );
        assert_eq!(
            results[OTHER]
                .as_ref()
                .expect("legacy type")
                .type_id
                .as_ref(),
            OTHER
        );
    }

    #[tokio::test]
    async fn persistent_failure_never_falls_back_to_a_legacy_root_copy() {
        let legacy =
            MockTypesRegistryClient::new().with_type_schemas([make_test_type_schema(ROOT)]);
        let registry = client(Arc::new(legacy), persistent_root(true));

        let error = registry
            .get_type_schema(ROOT)
            .await
            .expect_err("authoritative read must fail closed");

        assert!(
            error
                .to_string()
                .contains("persistent registry unavailable")
        );
    }

    #[tokio::test]
    async fn list_replaces_a_stale_legacy_root_with_the_authoritative_copy() {
        let legacy = MockTypesRegistryClient::new()
            .with_type_schemas([make_test_type_schema(ROOT), make_test_type_schema(OTHER)]);
        let registry = client(Arc::new(legacy), persistent_root(false));

        let schemas = registry
            .list_type_schemas(TypeSchemaQuery::new())
            .await
            .expect("list schemas");

        assert_eq!(schemas.len(), 2);
        let roots: Vec<_> = schemas
            .iter()
            .filter(|schema| schema.type_id.as_ref() == ROOT)
            .collect();
        assert_eq!(roots.len(), 1);
        assert_eq!(
            roots[0].effective_traits(),
            json!({"allowed_parent_types": [], "idp_provisioning": false})
        );
    }
}
