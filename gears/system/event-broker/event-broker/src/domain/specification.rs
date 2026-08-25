//! `SpecificationManager` (`DESIGN.md:721-737`): shared by Ingest and
//! Delivery, owns topic/event-type metadata. Signatures only.

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use crate::domain::error::DomainError;
use crate::domain::model::{EventType, Topic};

#[async_trait]
pub trait SpecificationManager: Send + Sync {
    async fn register_topic(&self, spec: Topic) -> Result<Topic, DomainError>;
    async fn register_event_type(&self, spec: EventType) -> Result<EventType, DomainError>;
    async fn get_topic(&self, gts_id: &str) -> Option<Topic>;
    async fn get_event_type(&self, gts_id: &str) -> Option<EventType>;
    async fn validate_event_data(
        &self,
        event_type: &EventType,
        data: &JsonValue,
    ) -> Result<(), DomainError>;
}
