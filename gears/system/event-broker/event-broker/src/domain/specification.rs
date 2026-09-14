//! `SpecificationManager` (`DESIGN.md:721-737`): shared by Ingest and
//! Delivery, owns topic/event-type metadata. Signatures only.

use async_trait::async_trait;
use gts::{GtsInstanceId, GtsTypeId};
use serde_json::Value as JsonValue;
use types_registry_sdk::{GtsInstance, GtsTypeSchema};

use crate::domain::error::DomainError;

/// A topic and an event type are both derived GTS type schemas, so every
/// signature here deals in the schema and in GTS *type* ids: neither identifier
/// can be an instance id, and there is no broker-owned record to build from
/// parts. The shapes the REST API reports are projections of these schemas,
/// computed where a value crosses the API boundary.
#[async_trait]
pub trait SpecificationManager: Send + Sync {
    async fn register_topic(&self, topic: GtsInstance) -> Result<GtsInstance, DomainError>;
    async fn register_event_type(&self, spec: GtsTypeSchema) -> Result<GtsTypeSchema, DomainError>;

    async fn get_topic(&self, topic: &GtsInstanceId) -> Option<GtsInstance>;
    async fn get_event_type(&self, event_type: &GtsTypeId) -> Option<GtsTypeSchema>;

    /// Validates `data` against the event type's payload contract - the type's
    /// narrowing of the base event's `data` member, which
    /// [`projection::event_type`](crate::domain::projection::event_type)
    /// composes - rather than against the whole resolved schema.
    async fn validate_event_data(
        &self,
        event_type: &GtsTypeSchema,
        data: &JsonValue,
    ) -> Result<(), DomainError>;
}
