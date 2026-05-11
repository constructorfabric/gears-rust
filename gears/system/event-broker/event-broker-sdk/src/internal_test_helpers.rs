//! Exposes internals needed by integration tests and the impl crate.
//! Not part of the stable public API.

pub use crate::internal::chain_state::ChainState;
pub use crate::internal::partitioning::{murmur3_32, partition_for};
pub use crate::models::Event as WireEvent;

use crate::ids::ProducerId;

pub fn chain_state_new() -> ChainState {
    ChainState::new()
}
pub fn chain_state_peek(cs: &ChainState, pid: ProducerId, topic: &str, partition: u32) -> i64 {
    cs.peek(&(pid, topic.to_owned(), partition))
}
pub fn chain_state_advance(
    cs: &ChainState,
    pid: ProducerId,
    topic: &str,
    partition: u32,
    seq: i64,
) {
    cs.advance((pid, topic.to_owned(), partition), seq);
}
pub fn chain_state_reset(cs: &ChainState, pid: ProducerId, topic: &str, partition: u32) {
    cs.reset(&(pid, topic.to_owned(), partition));
}
pub fn chain_state_bulk_prime(
    cs: &ChainState,
    entries: impl IntoIterator<Item = (ProducerId, String, u32, i64)>,
) {
    cs.bulk_prime(
        entries
            .into_iter()
            .map(|(pid, topic, partition, seq)| ((pid, topic, partition), seq)),
    );
}

// ---- Schema cache helpers ----

pub use crate::internal::schema_cache::SchemaCache as BareSchemaCache;

pub fn new_bare_schema_cache() -> crate::internal::schema_cache::SchemaCache {
    struct NopRegistry;
    #[async_trait::async_trait]
    impl types_registry_sdk::TypesRegistryClient for NopRegistry {
        async fn register(
            &self,
            _: Vec<serde_json::Value>,
        ) -> Result<
            Vec<types_registry_sdk::RegisterResult>,
            types_registry_sdk::error::TypesRegistryError,
        > {
            unimplemented!()
        }
        async fn register_type_schemas(
            &self,
            _: Vec<serde_json::Value>,
        ) -> Result<
            Vec<types_registry_sdk::RegisterResult>,
            types_registry_sdk::error::TypesRegistryError,
        > {
            unimplemented!()
        }
        async fn get_type_schema(
            &self,
            _: &str,
        ) -> Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::error::TypesRegistryError>
        {
            unimplemented!()
        }
        async fn get_type_schema_by_uuid(
            &self,
            _: uuid::Uuid,
        ) -> Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::error::TypesRegistryError>
        {
            unimplemented!()
        }
        async fn get_type_schemas(
            &self,
            _: Vec<String>,
        ) -> std::collections::HashMap<
            String,
            Result<
                types_registry_sdk::GtsTypeSchema,
                types_registry_sdk::error::TypesRegistryError,
            >,
        > {
            unimplemented!()
        }
        async fn get_type_schemas_by_uuid(
            &self,
            _: Vec<uuid::Uuid>,
        ) -> std::collections::HashMap<
            uuid::Uuid,
            Result<
                types_registry_sdk::GtsTypeSchema,
                types_registry_sdk::error::TypesRegistryError,
            >,
        > {
            unimplemented!()
        }
        async fn list_type_schemas(
            &self,
            _: types_registry_sdk::TypeSchemaQuery,
        ) -> Result<
            Vec<types_registry_sdk::GtsTypeSchema>,
            types_registry_sdk::error::TypesRegistryError,
        > {
            Ok(vec![])
        }
        async fn register_instances(
            &self,
            _: Vec<serde_json::Value>,
        ) -> Result<
            Vec<types_registry_sdk::RegisterResult>,
            types_registry_sdk::error::TypesRegistryError,
        > {
            unimplemented!()
        }
        async fn get_instance(
            &self,
            _: &str,
        ) -> Result<types_registry_sdk::GtsInstance, types_registry_sdk::error::TypesRegistryError>
        {
            unimplemented!()
        }
        async fn get_instance_by_uuid(
            &self,
            _: uuid::Uuid,
        ) -> Result<types_registry_sdk::GtsInstance, types_registry_sdk::error::TypesRegistryError>
        {
            unimplemented!()
        }
        async fn get_instances(
            &self,
            _: Vec<String>,
        ) -> std::collections::HashMap<
            String,
            Result<types_registry_sdk::GtsInstance, types_registry_sdk::error::TypesRegistryError>,
        > {
            unimplemented!()
        }
        async fn get_instances_by_uuid(
            &self,
            _: Vec<uuid::Uuid>,
        ) -> std::collections::HashMap<
            uuid::Uuid,
            Result<types_registry_sdk::GtsInstance, types_registry_sdk::error::TypesRegistryError>,
        > {
            unimplemented!()
        }
        async fn list_instances(
            &self,
            _: types_registry_sdk::InstanceQuery,
        ) -> Result<
            Vec<types_registry_sdk::GtsInstance>,
            types_registry_sdk::error::TypesRegistryError,
        > {
            unimplemented!()
        }
    }
    crate::internal::schema_cache::SchemaCache::new(std::sync::Arc::new(NopRegistry))
}

pub async fn schema_cache_seed(
    cache: &crate::internal::schema_cache::SchemaCache,
    type_id: &str,
    schema: serde_json::Value,
) {
    let resolved =
        crate::internal::schema_cache::ResolvedSchema::new_for_test(type_id.to_owned(), schema);
    cache
        .inner
        .write()
        .await
        .insert(type_id.to_owned(), std::sync::Arc::new(resolved));
}

pub async fn schema_cache_validate(
    cache: &crate::internal::schema_cache::SchemaCache,
    type_id: &str,
    data: &serde_json::Value,
) -> Result<(), crate::error::EventBrokerError> {
    cache.validate(type_id, data).await
}

pub async fn schema_cache_is_cached(
    cache: &crate::internal::schema_cache::SchemaCache,
    type_id: &str,
) -> bool {
    cache.is_cached(type_id).await
}
