//! Tool schema source over Type Registry (`TypesRegistryApi`, caller context).
//!
//! Reads the Type Schema for a GTS id under the caller's `SecurityContext`
//! (the projection must select the resolved schema document). Not found,
//! invisible to the caller and Type Registry failures all surface as
//! `schema_not_found` carrying the schema id (DESIGN §4.8).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use llm_gateway_sdk::LlmGatewayError;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsTypeId, Projection, ReadOptions, TypesRegistryApi,
    TypesRegistryApiExt as _,
};

use crate::config::DependencyTimeouts;
use crate::domain::ports::ToolSchemaSource;

/// [`ToolSchemaSource`] backed by the `TypesRegistryApi` client in `ClientHub`.
pub struct TypesRegistryToolSchemas {
    hub: Arc<ClientHub>,
    /// `timeouts.dependency.types_registry`: bound of one schema read. On
    /// expiry the read is `schema_not_found` like any other Type Registry
    /// outage.
    timeout: Duration,
}

impl TypesRegistryToolSchemas {
    /// Source resolving the Type Registry client from `hub` per call.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self {
            hub,
            timeout: DependencyTimeouts::default().types_registry,
        }
    }

    /// Bound one schema read by `timeout` (`timeouts.dependency.types_registry`).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl ToolSchemaSource for TypesRegistryToolSchemas {
    async fn schema(
        &self,
        ctx: &SecurityContext,
        schema_id: &str,
    ) -> Result<serde_json::Value, LlmGatewayError> {
        let not_found = || LlmGatewayError::schema_not_found(schema_id);

        let type_id = GtsTypeId::try_new(schema_id).map_err(|_| not_found())?;
        let registry = self.hub.get::<dyn TypesRegistryApi>().map_err(|error| {
            tracing::warn!(schema_id, %error, "type registry client unavailable");
            not_found()
        })?;
        let options = ReadOptions {
            projection: Projection::Select(FieldSelection::with(&[EntityField::ResolvedSchema])),
            ..ReadOptions::default()
        };
        // The caller's context: the registry applies its own visibility rules.
        let read = tokio::time::timeout(
            self.timeout,
            registry.get_type_schema(ctx, &type_id, options),
        )
        .await
        .map_err(|_| {
            tracing::warn!(
                schema_id,
                timeout_ms = self.timeout.as_millis(),
                "type registry schema read timed out"
            );
            not_found()
        })?;
        let snapshot = read.map_err(|error| {
            // Absent and invisible schemas are the caller's problem; any
            // other failure is Type Registry trouble worth an operator's
            // attention. The id is the only request datum logged.
            if !matches!(
                error,
                CanonicalError::NotFound { .. } | CanonicalError::PermissionDenied { .. }
            ) {
                tracing::warn!(schema_id, %error, "type registry schema read failed");
            }
            not_found()
        })?;
        snapshot.resolved_schema.ok_or_else(|| {
            tracing::warn!(schema_id, "type registry returned no resolved schema");
            not_found()
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "tool_schemas_tests.rs"]
mod tool_schemas_tests;
