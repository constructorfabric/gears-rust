//! Usage Collector client abstraction.
//!
//! @cpt-cf-file-storage-fr-usage-reporting

use async_trait::async_trait;
use uuid::Uuid;

/// A usage delta to report to the Usage Collector.
///
/// Positive `bytes_delta` = storage gain (upload/create).
/// Negative `bytes_delta` = storage freed (delete).
/// `file_count_delta`: +1 when a file is created, -1 when deleted, 0 otherwise.
///
/// @cpt-cf-file-storage-fr-usage-reporting
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct UsageDelta {
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
    pub bytes_delta: i64,
    pub file_count_delta: i64,
}

/// Usage reporting adapter — fire-and-forget; failures must NOT propagate to callers.
///
/// @cpt-cf-file-storage-fr-usage-reporting
#[async_trait]
pub trait UsageReporter: Send + Sync {
    /// Report a storage-delta event. Must be infallible from the caller's perspective —
    /// implementations MUST log and swallow errors internally.
    async fn report(&self, delta: UsageDelta);
}
