//! Centralized constants for metric label keys and values.
//!
//! Keeps magic strings out of service code and ensures consistent naming
//! across recording sites and dashboards.

// ── Label keys (OpenTelemetry attribute names) ───────────────────────────

pub mod key {
    pub const PROVIDER: &str = "provider";
    pub const MODEL: &str = "model";
    pub const ERROR_CODE: &str = "error_code";
    pub const STAGE: &str = "stage";
    pub const OP: &str = "op";
    pub const RESULT: &str = "result";
    pub const DECISION: &str = "decision";
    pub const PERIOD: &str = "period";
    pub const REASON: &str = "reason";
    pub const TRIGGER: &str = "trigger";
    pub const KIND: &str = "kind";
    pub const TIER: &str = "tier";
    pub const RESOURCE_TYPE: &str = "resource_type";
    #[allow(dead_code)] // declared ahead of call site (metrics infra uses string literals)
    pub const STATE: &str = "state";
    pub const PROVIDER_KIND: &str = "provider_kind";
}

// ── Label values ─────────────────────────────────────────────────────────

/// Turn mutation operation types (`op` label).
pub mod op {
    pub const RETRY: &str = "retry";
    pub const EDIT: &str = "edit";
    pub const DELETE: &str = "delete";
}

/// Mutation / generic result labels (`result` label).
pub mod result {
    pub const OK: &str = "ok";
    pub const NOT_LATEST: &str = "not_latest";
    pub const INVALID_STATE: &str = "invalid_state";
    pub const FORBIDDEN: &str = "forbidden";
    pub const GENERATION_IN_PROGRESS: &str = "generation_in_progress";
    pub const ERROR: &str = "error";
    /// Audit emit transient failure — will be retried by the outbox.
    pub const RETRY: &str = "retry";
    /// Audit emit permanent failure — dead-lettered by the outbox.
    pub const REJECT: &str = "reject";
    /// Audit event acknowledged without delivery: no audit plugin is
    /// registered.
    pub const DROPPED: &str = "dropped";
}

/// Quota preflight decision labels (`decision` label).
pub mod decision {
    pub const ALLOW: &str = "allow";
    pub const DOWNGRADE: &str = "downgrade";
    pub const REJECT: &str = "reject";
}

/// Quota / billing period labels (`period` label).
pub mod period {
    pub const DAILY: &str = "daily";
    pub const MONTHLY: &str = "monthly";
}

/// Disconnect / stream lifecycle stage labels (`stage` label).
pub mod stage {
    pub const BEFORE_FIRST_TOKEN: &str = "before_first_token";
    pub const MID_STREAM: &str = "mid_stream";
}

/// Attachment kind labels (`kind` label).
pub mod kind {
    pub const DOCUMENT: &str = "document";
    pub const IMAGE: &str = "image";
}

/// Attachment upload result labels (`result` label).
pub mod upload_result {
    pub const OK: &str = "ok";
    pub const FILE_TOO_LARGE: &str = "file_too_large";
    #[allow(dead_code)] // declared ahead of call site (deferred metrics)
    pub const UNSUPPORTED_TYPE: &str = "unsupported_type";
    pub const PROVIDER_ERROR: &str = "provider_error";
    pub const STORAGE_LIMIT_EXCEEDED: &str = "storage_limit_exceeded";
    pub const CONCURRENCY_LIMIT: &str = "concurrency_limit";
}

/// Background indexing result labels (`result` label of
/// `attachment_background_indexing`).
pub mod background_indexing_result {
    /// Indexing completed and the row is `ready`.
    pub const READY: &str = "ready";
    /// The provider reported `failed`/`cancelled`, or a status read failed
    /// with a non-transient error.
    pub const FAILED: &str = "failed";
    /// Indexing did not finish within the background timeout.
    pub const TIMEOUT: &str = "timeout";
    /// Indexing completed but the row could not be set `ready`.
    pub const SET_READY_FAILED: &str = "set_ready_failed";
}

/// Cleanup resource type labels (`resource_type` label).
pub mod resource_type {
    pub const FILE: &str = "file";
    pub const VECTOR_STORE: &str = "vector_store";
}

/// Cleanup retry reason labels (`reason` label of `cleanup_retry`).
/// Bounded values only; the error text goes to the log.
pub mod cleanup_retry_reason {
    /// The provider file delete failed.
    pub const PROVIDER_ERROR: &str = "provider_error";
    /// The provider vector store delete failed.
    pub const VECTOR_STORE_DELETE_FAILED: &str = "vector_store_delete_failed";
}

/// Cleanup backlog state labels (`state` label).
pub mod cleanup_state {
    #[allow(dead_code)] // declared ahead of call site (metrics infra uses string literals)
    pub const PENDING: &str = "pending";
    #[allow(dead_code)] // declared ahead of call site (metrics infra uses string literals)
    pub const FAILED: &str = "failed";
}

/// Orphan watchdog reason labels (`reason` label).
pub mod reason {
    pub const STALE_PROGRESS: &str = "stale_progress";
}

/// Cancel / abort trigger labels (`trigger` label).
pub mod trigger {
    #[allow(dead_code)] // declared ahead of call site (deferred metrics)
    pub const USER_STOP: &str = "user_stop";
    pub const DISCONNECT: &str = "disconnect";
    #[allow(dead_code)] // declared ahead of call site (deferred metrics)
    pub const TIMEOUT: &str = "timeout";
    pub const CLIENT_DISCONNECT: &str = "client_disconnect";
    pub const ORPHAN_TIMEOUT: &str = "orphan_timeout";
    pub const INTERNAL_ABORT: &str = "internal_abort";
}

/// Thread summary execution result labels (`result` label of
/// `thread_summary_execution`).
pub mod summary_result {
    pub const SUCCESS: &str = "success";
    pub const RETRY: &str = "retry";
    pub const PROVIDER_ERROR: &str = "provider_error";
    pub const EMPTY_SUMMARY: &str = "empty_summary";
    /// The summary model is missing or disabled in the catalog; the task is
    /// rejected.
    pub const MODEL_UNAVAILABLE: &str = "model_unavailable";
    /// The target frontier message was deleted while the summary was
    /// generated; nothing is committed.
    pub const FRONTIER_DELETED: &str = "frontier_deleted";
    /// The task expected a stored summary that no longer exists (dropped by
    /// a retry, edit or delete); nothing is committed.
    pub const BASE_MISSING: &str = "base_missing";
}
