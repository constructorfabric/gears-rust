//! Data types for the transactional outbox pattern.

use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

/// Message to be enqueued into the outbox table.
///
/// Producers create an `OutboxMessage` and pass it to [`enqueue`](super::enqueue)
/// inside the same DB transaction as their domain side effects.
#[derive(Debug, Clone)]
pub struct OutboxMessage {
    /// Namespace for routing (e.g., `"mini-chat"`).
    pub namespace: &'static str,
    /// Topic within the namespace (e.g., `"usage-settlement"`).
    pub topic: &'static str,
    /// Optional tenant ID for routing and filtering.
    pub tenant_id: Option<Uuid>,
    /// Optional idempotency key. When provided, the partial unique index on
    /// `(namespace, topic, dedupe_key)` ensures at-most-once enqueue.
    /// The key format is producer-defined and domain-specific.
    pub dedupe_key: Option<String>,
    /// Payload containing all information needed by downstream consumers.
    pub payload: Value,
}

/// Configuration for claiming a batch of outbox rows.
#[derive(Debug, Clone, Copy)]
pub struct ClaimCfg {
    /// Maximum number of rows to claim in one batch.
    pub batch_size: u32,
    /// Duration of the lease. Rows become reclaimable after this expires.
    pub lease_duration: Duration,
}

/// A row claimed from the outbox, ready for publishing.
#[derive(Debug, Clone)]
pub struct ClaimedMessage {
    /// Row primary key.
    pub id: Uuid,
    /// Namespace the row belongs to.
    pub namespace: String,
    /// Topic within the namespace.
    pub topic: String,
    /// Optional tenant ID.
    pub tenant_id: Option<Uuid>,
    /// Optional idempotency / dedupe key.
    pub dedupe_key: Option<String>,
    /// Event payload.
    pub payload: Value,
    /// Total delivery attempts so far (incremented at claim time).
    pub attempts: i32,
}

/// Retry policy configuration for the outbox dispatcher.
///
/// Used by [`OutboxStore::nack`](super::OutboxStore::nack) to compute
/// `next_attempt_at` with exponential backoff and jitter, and to
/// determine when a row should be dead-lettered.
#[derive(Debug, Clone, Copy)]
pub struct RetryCfg {
    /// Maximum total delivery attempts (including the first).
    /// When `attempts >= max_attempts`, the row transitions to `dead`.
    pub max_attempts: u32,
    /// Minimum retry interval (e.g., 1 second).
    pub base_delay: Duration,
    /// Upper bound on computed delay before jitter (e.g., 300 seconds).
    pub max_delay: Duration,
}

/// Outbox row status.
///
/// Represents the state machine: `pending` -> `processing` -> `delivered` | `dead`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    /// Row is eligible for claiming when `next_attempt_at <= now()`.
    Pending,
    /// Row is claimed by a dispatcher and has an active lease.
    Processing,
    /// Terminal: successfully delivered.
    Delivered,
    /// Terminal: permanent failure (attempts exceeded `max_attempts`).
    Dead,
}

impl OutboxStatus {
    /// SQL string representation.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Delivered => "delivered",
            Self::Dead => "dead",
        }
    }
}

impl std::fmt::Display for OutboxStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for OutboxStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "delivered" => Ok(Self::Delivered),
            "dead" => Ok(Self::Dead),
            other => Err(format!("unknown outbox status: {other}")),
        }
    }
}
