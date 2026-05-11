use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use tokio::sync::RwLock;
use types_registry_sdk::{TypeSchemaQuery, TypesRegistryClient};

use crate::error::EventBrokerError;

/// Cached entry: the JSON Schema for an event type's `data` field.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedSchema {
    pub type_id: String,
    /// The JSON Schema used to validate `event.data`.
    pub data_schema: serde_json::Value,
    /// Compiled jsonschema validator (boxed to keep the public type independent of jsonschema version).
    validator: Arc<jsonschema::Validator>,
}

impl ResolvedSchema {
    #[doc(hidden)]
    pub fn new_for_test(type_id: String, data_schema: serde_json::Value) -> Self {
        Self::new(type_id, data_schema).expect("test schema should compile")
    }

    fn new(type_id: String, data_schema: serde_json::Value) -> Result<Self, EventBrokerError> {
        let validator = jsonschema::validator_for(&data_schema).map_err(|e| {
            EventBrokerError::Internal(format!("compile schema for {type_id}: {e}"))
        })?;
        Ok(Self {
            type_id,
            data_schema,
            validator: Arc::new(validator),
        })
    }

    pub(crate) fn validate(&self, data: &serde_json::Value) -> Result<(), Vec<String>> {
        let errors: Vec<String> = self
            .validator
            .iter_errors(data)
            .map(|e| e.to_string())
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Schema cache backed by `types-registry-sdk`.
pub struct SchemaCache {
    #[doc(hidden)]
    pub inner: RwLock<HashMap<String, Arc<ResolvedSchema>>>,
    client: Arc<dyn TypesRegistryClient>,
}

impl SchemaCache {
    pub(crate) fn new(client: Arc<dyn TypesRegistryClient>) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            client,
        }
    }

    /// Resolve a single event-type id: fetch from types-registry if not cached.
    pub(crate) async fn resolve_one(
        &self,
        _ctx: &SecurityContext,
        type_id: &str,
    ) -> Result<Arc<ResolvedSchema>, EventBrokerError> {
        // Fast path.
        if let Some(s) = self.inner.read().await.get(type_id) {
            return Ok(s.clone());
        }
        // Slow path: fetch.
        let schemas = self.client.get_type_schema(type_id).await.map_err(|e| {
            EventBrokerError::EventTypeUnknown {
                type_id: type_id.to_owned(),
                detail: e.to_string(),
                instance: String::new(),
            }
        })?;

        // The `raw_schema` is the GTS type schema. For event types the data
        // schema is embedded under `x-event-data-schema` (if the broker
        // registered it that way) or we use the raw_schema directly as the
        // validator for `event.data`.
        let data_schema = schemas.raw_schema.clone();
        let resolved = Arc::new(ResolvedSchema::new(type_id.to_owned(), data_schema)?);
        self.inner
            .write()
            .await
            .insert(type_id.to_owned(), resolved.clone());
        Ok(resolved)
    }

    /// Expand one GTS wildcard pattern and cache all matching schemas.
    pub(crate) async fn resolve_pattern(
        &self,
        _ctx: &SecurityContext,
        pattern: &str,
    ) -> Result<Vec<String>, EventBrokerError> {
        let schemas = self
            .client
            .list_type_schemas(TypeSchemaQuery::default().with_pattern(pattern))
            .await
            .map_err(|e| EventBrokerError::EventTypeUnknown {
                type_id: pattern.to_owned(),
                detail: e.to_string(),
                instance: String::new(),
            })?;

        if schemas.is_empty() {
            return Err(EventBrokerError::EventTypeUnknown {
                type_id: pattern.to_owned(),
                detail: "pattern matched zero registered types".into(),
                instance: String::new(),
            });
        }

        let mut type_ids = Vec::with_capacity(schemas.len());
        let mut guard = self.inner.write().await;
        for schema in schemas {
            let type_id = schema.type_id.to_string();
            let data_schema = schema.raw_schema;
            match ResolvedSchema::new(type_id.clone(), data_schema) {
                Ok(resolved) => {
                    guard.insert(type_id.clone(), Arc::new(resolved));
                    type_ids.push(type_id);
                }
                Err(e) => {
                    tracing::warn!(type_id = %type_id, error = %e, "failed to compile schema");
                }
            }
        }
        Ok(type_ids)
    }

    /// Expand all patterns. Used for eager validation at build time.
    pub(crate) async fn resolve_all_patterns(
        &self,
        ctx: &SecurityContext,
        patterns: &[String],
    ) -> Result<Vec<String>, EventBrokerError> {
        let mut all = Vec::new();
        for pattern in patterns {
            let ids = self.resolve_pattern(ctx, pattern).await?;
            all.extend(ids);
        }
        Ok(all)
    }

    /// Check whether a `type_id` is currently cached.
    pub(crate) async fn is_cached(&self, type_id: &str) -> bool {
        self.inner.read().await.contains_key(type_id)
    }

    /// Validate `data` against the cached schema for `type_id`.
    /// Panics in debug mode if `type_id` is not cached (callers must ensure pre-fetch).
    pub(crate) async fn validate(
        &self,
        type_id: &str,
        data: &serde_json::Value,
    ) -> Result<(), EventBrokerError> {
        let guard = self.inner.read().await;
        let schema = guard.get(type_id).ok_or_else(|| {
            EventBrokerError::Internal(format!("schema not cached for {type_id}; this is a bug"))
        })?;
        schema
            .validate(data)
            .map_err(|errors| EventBrokerError::EventDataInvalid {
                type_id: type_id.to_owned(),
                errors,
                detail: "event payload failed schema validation".into(),
                instance: String::new(),
            })
    }
}
