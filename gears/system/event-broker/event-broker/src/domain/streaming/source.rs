//! How a session addresses a partition.
//!
//! Only the key lives here now. The read seam is `PartitionReader` (D27), and
//! there is no per-partition factory trait beside it: a session attaches to a
//! whole assignment at once, and the partitions it needs may not exist yet, so
//! only `infra::loader::topics::TopicManager` can create them. A
//! `PartitionSource` trait declaring `open_reader(offset)` stood here briefly
//! and never acquired a caller - `infra::loader::attach::attach_readers` is the
//! attach point, and it works at assignment scope rather than per partition.

use std::sync::Arc;

use tokio::sync::Notify;
use toolkit_gts::GtsInstanceId;

use crate::domain::model::{Assignment, Cursor};
use crate::domain::streaming::read_set::PartitionSlot;

/// The domain's port for attaching partition readers to a subscription's
/// assignment. `DeliveryServiceImpl` depends on this rather than the concrete
/// `infra::loader::topics::TopicManager`, keeping the domain free of infra types
/// (the same trait-in-`domain`, impl-in-`infra` shape as
/// [`crate::domain::backend::BackendResolver`] and
/// [`crate::domain::specification::SpecificationManager`]). `attach_readers`
/// (`infra::loader::attach`) is the implementation.
pub trait ReaderAttacher: Send + Sync {
    /// Open a reader per assigned partition at its persisted cursor, sharing
    /// `ready` so the session awaits once for the whole assignment. All
    /// partition-cache machinery stays behind the implementation; only domain
    /// [`PartitionSlot`]s cross back.
    fn attach_readers(
        &self,
        assigned: &[Assignment],
        cursors: &[Cursor],
        ready: &Arc<Notify>,
    ) -> Vec<PartitionSlot>;
}

/// One partition of one topic - the unit every read, resident span, and reader
/// registration is scoped to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PartitionKey {
    pub topic: GtsInstanceId,
    pub partition: i32,
}

impl PartitionKey {
    #[must_use]
    pub fn new(topic: GtsInstanceId, partition: i32) -> Self {
        Self { topic, partition }
    }
}
