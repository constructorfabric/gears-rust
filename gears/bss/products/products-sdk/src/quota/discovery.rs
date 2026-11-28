//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-contract:p1
//! The quota discovery plugin contract (P-D-267). An adapter implements
//! [`ProductsQuotaDiscoveryPluginClientV1`], registers its plugin instance under
//! [`DISCOVERY_PLUGIN_TYPE`](super::gts::DISCOVERY_PLUGIN_TYPE), and registers a scoped client
//! under that instance id. Products reads one complete snapshot of a target page by page and
//! verifies its count and digest ([`snapshot_digest`](super::digest::snapshot_digest)).
use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The largest page an adapter is asked for.
pub const MAX_PAGE_SIZE: u32 = 200;
/// The page size Products asks for when its configuration names none.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// One page request to an adapter target (quotas DESIGN §3.3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaDiscoveryRequest {
    /// The saved target's opaque key.
    pub target_key: String,
    /// `None` for the first page.
    pub cursor: Option<String>,
    /// 1 … 200; Products sends its configured size (default 50).
    pub page_size: u32,
}

/// One page of one immutable snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaDiscoveryPage {
    /// The snapshot every page of one read names.
    pub snapshot_id: Uuid,
    /// Non-negative decimal integer, monotonic per target.
    pub source_revision: String,
    /// When the adapter observed the snapshot.
    pub observed_at: OffsetDateTime,
    /// Whole-snapshot count.
    pub item_count: u64,
    /// Whole-snapshot canonical SHA-256 (`digest::snapshot_digest`), lowercase hex.
    pub content_digest: String,
    /// Raw capability records; Products validates each against its registered schema.
    pub items: Vec<serde_json::Value>,
    /// The cursor of the next page; `None` on the last page.
    pub next_cursor: Option<String>,
}

/// Read-only capability discovery; adapters implement it and register a scoped client under their
/// plugin instance id (P-D-267).
///
/// What an adapter guarantees (quotas DESIGN §3.3.1, §3.3.2):
///
/// - a snapshot is immutable and covers one target; its `source_revision` is monotonic for that
///   target;
/// - every page of one read repeats the snapshot's `snapshot_id`, `source_revision`,
///   `observed_at`, `item_count` and `content_digest`, and a read never mixes pages of two
///   snapshots or two revisions;
/// - a continuation cursor binds the caller's authorized scope, the target and the snapshot;
/// - a snapshot that is no longer held is an error, never a shorter page: Products then starts
///   the discovery again from the first page.
///
/// What Products accepts of the pages (P-D-268):
///
/// - a page may hold fewer records than `page_size` asks for; only the number of records bounds a
///   read, and a snapshot of more records than Products' configured bound is incomplete;
/// - every page but the last holds at least one record: an empty page with a `next_cursor` is an
///   incomplete snapshot, so a read always ends.
///
/// Products treats a failed or incomplete read as no answer, never as capabilities removed.
#[async_trait]
pub trait ProductsQuotaDiscoveryPluginClientV1: Send + Sync {
    /// One page of the current snapshot of `request.target_key`.
    ///
    /// # Errors
    /// A canonical error; Products answers any error 503 `QUOTA_DISCOVERY_UNAVAILABLE`.
    async fn list_capabilities(
        &self,
        ctx: &SecurityContext,
        request: QuotaDiscoveryRequest,
    ) -> Result<QuotaDiscoveryPage, CanonicalError>;
}
