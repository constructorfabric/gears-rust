//! `IngestService` (`DESIGN.md:623-639`): producer-facing, owns the write
//! path. Signatures only; bodies land with #4346 (REST API implementation)
//! and #4347 (standalone runtime).

use async_trait::async_trait;
use toolkit::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::Event;

/// Result of a batch publish - which events were accepted (in submission
/// order) and which were rejected with a reason. Exact shape is finalized
/// alongside the REST DTOs in #4346.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct BatchResult {
    pub accepted: Vec<Uuid>,
    pub failed: Vec<(Uuid, String)>,
}

#[async_trait]
pub trait IngestService: Send + Sync {
    /// Validate, sequence, and enqueue one event for persistence
    /// (`DESIGN.md:638`).
    async fn publish_event(
        &self,
        ctx: &SecurityContext,
        event: Event,
    ) -> Result<Event, DomainError>;

    /// Validate and enqueue a batch of events whose event types all resolve to
    /// the same topic (`DESIGN.md:639`). Events name no topic themselves, so
    /// batch homogeneity is a property of the resolved types, not of a field
    /// the producer set.
    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: Vec<Event>,
    ) -> Result<BatchResult, DomainError>;
}
