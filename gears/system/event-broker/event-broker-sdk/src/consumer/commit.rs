use crate::error::ConsumerError;

/// Commit handle for async-commit consumers. Only `commit` is available.
/// Calling `commit_in_tx` on this type is a compile error.
pub struct CommitHandle {
    pub(crate) partition: u32,
    pub(crate) offset: i64,
    /// Set to true when `commit` is called; dispatcher reads this to skip auto-commit.
    pub(crate) committed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl CommitHandle {
    pub(crate) fn new(partition: u32, offset: i64) -> Self {
        Self {
            partition,
            offset,
            committed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Mark this offset as processed. The dispatcher's auto-commit timer persists
    /// it via the offset store. Returns immediately (non-blocking).
    pub async fn commit(&self) -> Result<(), ConsumerError> {
        self.committed
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

/// Commit handle for tx-capable consumers (`outbox` feature).
/// Offers `commit_in_tx`, which writes the offset into the caller's transaction
/// atomically with the handler's business writes.
///
/// Generic over `OM: CommitOffsetInTx` so `commit_in_tx` can call
/// `OM::commit_in_tx` without boxing. Handler impls use the concrete OM type
/// (`TxCommitHandle<LocalDbOffsetManager>`).
#[cfg(feature = "db")]
pub struct TxCommitHandle<OM: super::CommitOffsetInTx> {
    pub(crate) partition: u32,
    pub(crate) offset: i64,
    pub(crate) offset_manager: std::sync::Arc<OM>,
    pub(crate) group: crate::ids::ConsumerGroupId,
    pub(crate) topic: crate::ids::TopicId,
    /// True when `commit_in_tx` succeeded; dispatcher skips the next auto-commit for this offset.
    pub(crate) committed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "db")]
impl<OM: super::CommitOffsetInTx> TxCommitHandle<OM> {
    pub(crate) fn new(
        partition: u32,
        offset: i64,
        offset_manager: std::sync::Arc<OM>,
        group: crate::ids::ConsumerGroupId,
        topic: crate::ids::TopicId,
    ) -> Self {
        Self {
            partition,
            offset,
            offset_manager,
            group,
            topic,
            committed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Persist the offset inside the caller's transaction (atomic with the handler's
    /// DB writes). After this call succeeds, the dispatcher will NOT auto-commit this
    /// offset - the cursor is already advanced in the caller's txn.
    pub async fn commit_in_tx<TX>(&self, txn: &TX) -> Result<(), ConsumerError>
    where
        TX: toolkit_db::secure::DBRunner + Sync,
    {
        self.offset_manager
            .commit_in_tx(txn, &self.group, &self.topic, self.partition, self.offset)
            .await
            .map_err(crate::error::EventBrokerError::OffsetManager)?;
        self.committed
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}
