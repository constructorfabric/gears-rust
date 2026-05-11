use crate::error::ConsumerError;

/// Ack handle for broker-only consumers. Only `commit_on_eb` is available.
/// Calling `commit_in_tx` on this type is a compile error.
pub struct AckHandle {
    pub(crate) partition: u32,
    pub(crate) offset: i64,
    /// Set to true when `commit_on_eb` is called; dispatcher reads this to skip auto-ack.
    pub(crate) committed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl AckHandle {
    pub(crate) fn new(partition: u32, offset: i64) -> Self {
        Self {
            partition,
            offset,
            committed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Mark this offset for batched broker ack. Returns immediately (non-blocking).
    pub async fn commit_on_eb(&self) -> Result<(), ConsumerError> {
        self.committed
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

/// Ack handle for tx-capable consumers (`outbox` feature).
/// Adds `commit_in_tx` which writes the offset into the caller's transaction atomically.
///
/// Generic over `OM: TxOffsetManager` so `commit_in_tx` can call
/// `OM::save_in_tx` without boxing. Handler impls use the concrete OM type
/// (`TxAckHandle<LocalDbOffsetManager>`).
#[cfg(feature = "outbox")]
pub struct TxAckHandle<OM: super::TxOffsetManager> {
    pub(crate) partition: u32,
    pub(crate) offset: i64,
    pub(crate) offset_manager: std::sync::Arc<OM>,
    pub(crate) group: crate::ids::ConsumerGroupId,
    pub(crate) topic: String,
    /// True when `commit_in_tx` succeeded; dispatcher skips the next auto-ack for this offset.
    pub(crate) committed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "outbox")]
impl<OM: super::TxOffsetManager> TxAckHandle<OM> {
    pub(crate) fn new(
        partition: u32,
        offset: i64,
        offset_manager: std::sync::Arc<OM>,
        group: crate::ids::ConsumerGroupId,
        topic: String,
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

    /// Mark this offset for batched broker ack. Non-blocking.
    pub async fn commit_on_eb(&self) -> Result<(), ConsumerError> {
        self.committed
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// Persist the offset inside the caller's transaction (atomic with handler's DB writes).
    ///
    /// After this call succeeds, the dispatcher will NOT emit a `save_on_eb` for this
    /// offset — the cursor is already advanced in the caller's txn.
    pub async fn commit_in_tx<TX>(&self, txn: &TX) -> Result<(), ConsumerError>
    where
        TX: toolkit_db::secure::DBRunner + Sync + ?Sized,
    {
        self.offset_manager
            .save_in_tx(txn, &self.group, &self.topic, self.partition, self.offset)
            .await
            .map_err(crate::error::EventBrokerError::OffsetManager)?;
        self.committed
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}
