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
    #[allow(dead_code)] // declared ahead of call site (deferred metrics)
    pub const FILE_TOO_LARGE: &str = "file_too_large";
    #[allow(dead_code)] // declared ahead of call site (deferred metrics)
    pub const UNSUPPORTED_TYPE: &str = "unsupported_type";
    pub const PROVIDER_ERROR: &str = "provider_error";
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
