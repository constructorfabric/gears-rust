//! The traits a storage backend plugin implements (feature 0007 §2.1, §2.7).

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::append::{AppendRequest, Appended, Dedup};
use super::error::BackendError;
use super::ids::TopicPartition;
use super::provision::{LogClaim, Provisioning, TopicSpec};
use super::read::Wait;
use super::watch::Watch;
use crate::GtsTypeId;
use crate::api::Position;
use crate::models::{Event, PartitionRange, TopicSegment};
use crate::sequence::Sequence;

/// The trait every storage backend plugin implements.
///
/// The backend owns a topic's log: it picks each event's partition from the
/// event's key and assigns its sequence. Ingest hands events over and learns
/// partition heads afterwards; delivery reads by offset. Retention is the
/// backend's own: it removes expired events on its own schedule from the
/// bounds `provision` handed it, ageing them by `sequence_time` (when it stored
/// them), and the gear never calls into it to sweep.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Creates whatever the backend needs to hold the topic's log and stores
    /// `claim.new` as its log id; compare-and-set per [`LogClaim::accepts`],
    /// anything else is [`BackendError::LogConflict`]. Called again with the
    /// same log id when the topic's retention changes, and the backend adopts
    /// the new bounds.
    async fn provision(
        &self,
        topic: &TopicSpec,
        claim: &LogClaim,
    ) -> Result<Provisioning, BackendError>;

    /// Stores the request's events, returning only once every one of them is
    /// durable; exactly once per [`Fence`](super::Fence) unless [`dedup`](Self::dedup)
    /// is [`Dedup::AtLeastOnce`].
    async fn append(&self, request: &AppendRequest<'_>) -> Result<Appended, BackendError>;

    /// What deduplication this backend guarantees; declared once and logged at
    /// startup.
    fn dedup(&self) -> Dedup;

    /// The most events one `append` may carry; ingest never exceeds it.
    fn max_events_per_append(&self) -> usize;

    /// Events of `at` with sequence greater than `offset`, ascending, at most
    /// `max_events`. `Sequence::NONE` reads from the start.
    async fn read(
        &self,
        at: &TopicPartition,
        offset: Sequence,
        max_events: usize,
        wait: Wait,
    ) -> Result<Vec<Event>, BackendError>;

    /// `Exact`, `Earliest`, `Latest`, `At(t)` to the offset a cursor may hold.
    /// `Latest` is the highest sequence ever assigned, never lowered by
    /// removal.
    async fn resolve(
        &self,
        at: &TopicPartition,
        position: Position,
    ) -> Result<Sequence, BackendError>;

    /// How head changes are observed: pushed by the medium, or polled.
    fn watch(&self) -> Watch<'_>;

    async fn query(
        &self,
        at: &TopicPartition,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError>;
}

/// Builds backends of one GTS backend type from a registry entry's settings.
#[async_trait]
pub trait Provider: Send + Sync {
    /// The GTS backend type this provider builds, derived from
    /// `gts.cf.core.events.backend.v1~`.
    fn backend_type(&self) -> &GtsTypeId;

    /// Builds one registry entry (feature 0005 §2.1) from the settings beside
    /// its `type`.
    ///
    /// Validates the settings and connects lazily: an unreachable medium is a
    /// failed operation (`Unavailable`), never a failed build. The backend's
    /// own workers run until `stop` is cancelled. An error names the setting
    /// and the rule it breaks, never the setting's value.
    async fn build(
        &self,
        settings: &serde_json::Map<String, serde_json::Value>,
        stop: CancellationToken,
    ) -> Result<Arc<dyn Backend>, BackendError>;
}
