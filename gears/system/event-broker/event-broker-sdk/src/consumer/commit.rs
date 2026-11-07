#[cfg(feature = "db")]
use crate::error::ConsumerError;
#[cfg(feature = "db")]
use crate::sequence::Sequence;

/// Commit handle for tx-capable consumers (`db` feature).
/// Offers `commit_offset_in_tx`, which writes a delivered offset into the
/// caller's transaction atomically with the handler's business writes.
///
/// Generic over `OM: CommitOffsetInTx` so `commit_offset_in_tx` can call
/// `OM::commit_in_tx` without boxing. Handler impls use the concrete OM type
/// (`TxCommitHandle<LocalDbOffsetManager>`).
#[cfg(feature = "db")]
pub struct TxCommitHandle<OM: super::CommitOffsetInTx> {
    /// Partition the delivered batch was read from.
    pub(crate) partition: u32,
    /// Offsets this handle may commit, in delivery order: the delivered batch's, or
    /// those of the part of it one handler got.
    pub(crate) batch_offsets: Vec<Sequence>,
    /// Offset store that writes into the caller's transaction.
    pub(crate) offset_manager: std::sync::Arc<OM>,
    /// Consumer group whose offset is committed.
    pub(crate) group: crate::ids::ConsumerGroupId,
    /// Topic whose offset is committed.
    pub(crate) topic: crate::ids::TopicId,
    /// Offset successfully written inside the user transaction.
    pub(crate) committed_offset: std::sync::Arc<std::sync::Mutex<Option<Sequence>>>,
}

#[cfg(feature = "db")]
pub(crate) struct TxCommitHandleParts<OM: super::CommitOffsetInTx> {
    pub(crate) partition: u32,
    pub(crate) offsets: Vec<Sequence>,
    pub(crate) offset_manager: std::sync::Arc<OM>,
    pub(crate) group: crate::ids::ConsumerGroupId,
    pub(crate) topic: crate::ids::TopicId,
}

#[cfg(feature = "db")]
impl<OM: super::CommitOffsetInTx> TxCommitHandle<OM> {
    /// Assembles a handle from the runtime-provided parts.
    pub(crate) fn new(parts: TxCommitHandleParts<OM>) -> Self {
        Self {
            partition: parts.partition,
            batch_offsets: parts.offsets,
            offset_manager: parts.offset_manager,
            group: parts.group,
            topic: parts.topic,
            committed_offset: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// A handle over `offsets` only, for the part of a batch one handler gets. It
    /// shares this handle's commit state, so a commit through it is what the runtime
    /// reads for the whole batch.
    pub(crate) fn scoped(&self, offsets: Vec<Sequence>) -> Self {
        Self {
            partition: self.partition,
            batch_offsets: offsets,
            offset_manager: std::sync::Arc::clone(&self.offset_manager),
            group: self.group,
            topic: self.topic,
            committed_offset: std::sync::Arc::clone(&self.committed_offset),
        }
    }

    /// Writes `offset` as this consumer's committed position for the partition inside
    /// `txn`, so the position commits or rolls back with the handler's own writes, and
    /// records it as what the runtime advances through once the handler succeeds.
    ///
    /// It is recorded when the offset manager's write returns, before `txn` commits. A
    /// handler whose transaction then rolls back must not return `Success` (return
    /// `Retry` or an error): on `Success` the runtime moves past the events through
    /// `offset` and reports [`ConsumerRuntimeEvent::OffsetCommitted`] although nothing was
    /// stored.
    ///
    /// # Errors
    ///
    /// - [`InvalidConsumerOptions`](crate::EventBrokerError::InvalidConsumerOptions) if
    ///   `offset` is not one of the offsets this handle was given; nothing is written.
    /// - [`OffsetManager`](crate::EventBrokerError::OffsetManager) if the offset manager
    ///   fails to write it.
    /// - [`Internal`](crate::EventBrokerError::Internal) if the shared commit state is
    ///   poisoned, which happens only after a thread panicked while holding it (a broken
    ///   invariant).
    ///
    /// [`ConsumerRuntimeEvent::OffsetCommitted`]: super::ConsumerRuntimeEvent::OffsetCommitted
    pub async fn commit_offset_in_tx<TX>(
        &self,
        txn: &TX,
        offset: Sequence,
    ) -> Result<(), ConsumerError>
    where
        TX: toolkit_db::secure::DBRunner + Sync,
    {
        if !self.batch_offsets.contains(&offset) {
            return Err(crate::error::EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "committed offset {offset} is not present in delivered batch offsets {:?}",
                    self.batch_offsets
                ),
            });
        }

        self.offset_manager
            .commit_in_tx(txn, &self.group, &self.topic, self.partition, offset)
            .await
            .map_err(crate::error::EventBrokerError::OffsetManager)?;
        *self.committed_offset.lock().map_err(|_| {
            crate::error::EventBrokerError::Internal("tx commit state mutex poisoned".into())
        })? = Some(offset);
        Ok(())
    }
}
