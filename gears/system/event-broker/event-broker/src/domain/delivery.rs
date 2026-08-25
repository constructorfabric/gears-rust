//! `DeliveryService` (`DESIGN.md:641-658`): consumer-facing, owns the read
//! path and subscription lifecycle. Signatures only; bodies land with #4346
//! (REST API implementation) and #4347 (standalone runtime).

use async_trait::async_trait;
use toolkit::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::{Event, Subscription};

/// JOIN request body (`DESIGN.md:692`: `consumer_group` + `interests[]`).
/// Exact shape (typed filters per ADR-0005) is finalized alongside the REST
/// DTOs in #4346.
#[domain_model]
#[derive(Debug, Clone)]
pub struct JoinRequest {
    pub consumer_group: String,
    pub topics: Vec<String>,
}

/// Long-poll response - events plus topology-version-aware metadata
/// (`DESIGN.md:657`). Exact shape finalized in #4346.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct PollResponse {
    pub events: Vec<Event>,
    pub topology_version: i64,
}

/// One SEEK target. A subscription can span multiple topics, and
/// assignments/cursors are identified by the full `(topic, partition)`
/// pair (`DESIGN.md:658`) - keying by `partition` alone would collapse
/// partition `0` of two different topics into one entry.
#[domain_model]
#[derive(Debug, Clone)]
pub struct SeekPosition {
    pub topic: String,
    pub partition: i32,
    pub offset: i64,
}

#[async_trait]
pub trait DeliveryService: Send + Sync {
    /// Creates a subscription in cache, claims/joins the group
    /// (`DESIGN.md:655`).
    async fn join(
        &self,
        ctx: &SecurityContext,
        request: JoinRequest,
    ) -> Result<Subscription, DomainError>;

    /// Removes the subscription from its group, triggers rebalance
    /// (`DESIGN.md:656`).
    async fn leave(&self, ctx: &SecurityContext, subscription_id: Uuid) -> Result<(), DomainError>;

    /// Long-poll with topology-version-aware response (`DESIGN.md:657`).
    async fn poll(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
        timeout: std::time::Duration,
    ) -> Result<PollResponse, DomainError>;

    /// Sets the cursor position for each assigned `(topic, partition)`
    /// (`DESIGN.md:658`).
    async fn seek(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
        positions: Vec<SeekPosition>,
    ) -> Result<(), DomainError>;
}
