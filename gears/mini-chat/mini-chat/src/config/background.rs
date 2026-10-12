use serde::{Deserialize, Serialize};

/// Upper bound of the `scan_interval_secs` of the periodic workers. A larger
/// value makes the worker scan once at startup and then practically never.
pub const MAX_SCAN_INTERVAL_SECS: u64 = 3600;

/// Orphan watchdog — detects and finalizes turns abandoned by crashed pods.
///
/// Requires leader election (exactly one active instance per environment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanWatchdogConfig {
    /// Enable the orphan watchdog. Default: `true`.
    #[serde(default = "super::default_true")]
    pub enabled: bool,
    /// Scan interval in seconds. Default: 60.
    #[serde(default = "default_orphan_scan_interval")]
    pub scan_interval_secs: u64,
    /// A `running` turn with `last_progress_at` older than this is orphan-eligible.
    /// Valid range: 90–3600. Default: 300 (5 min).
    /// Minimum 90s = 3× `PROGRESS_UPDATE_INTERVAL` (30s) to avoid false orphaning.
    #[serde(default = "default_orphan_timeout")]
    pub timeout_secs: u64,
}

impl Default for OrphanWatchdogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_interval_secs: default_orphan_scan_interval(),
            timeout_secs: default_orphan_timeout(),
        }
    }
}

impl OrphanWatchdogConfig {
    /// Minimum timeout to avoid false orphaning under normal jitter.
    /// `PROGRESS_UPDATE_INTERVAL` is 30s; 90s gives 3 heartbeat windows of headroom.
    const MIN_TIMEOUT_SECS: u64 = 90;

    pub fn validate(&self) -> Result<(), String> {
        if !(Self::MIN_TIMEOUT_SECS..=3600).contains(&self.timeout_secs) {
            return Err(format!(
                "orphan_watchdog.timeout_secs must be {}-3600, got {}",
                Self::MIN_TIMEOUT_SECS,
                self.timeout_secs
            ));
        }
        if !(1..=MAX_SCAN_INTERVAL_SECS).contains(&self.scan_interval_secs) {
            return Err(format!(
                "orphan_watchdog.scan_interval_secs must be 1-{MAX_SCAN_INTERVAL_SECS}, got {}",
                self.scan_interval_secs
            ));
        }
        Ok(())
    }
}

fn default_orphan_scan_interval() -> u64 {
    60
}

/// Upload reaper — marks attachments stuck in `pending` / `uploaded` as
/// `failed` (`upload_abandoned`) and schedules the provider file delete.
/// Rows get stuck when the upload request is dropped (client disconnect,
/// api-gateway timeout) before the service records the outcome.
///
/// Requires leader election (exactly one active instance per environment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadReaperConfig {
    /// Enable the upload reaper. Default: `true`.
    #[serde(default = "super::default_true")]
    pub enabled: bool,
    /// Scan interval in seconds. Default: 60.
    #[serde(default = "default_upload_reaper_scan_interval")]
    pub scan_interval_secs: u64,
    /// A `pending` / `uploaded` row whose `updated_at` is older than this is
    /// abandoned. Valid range: 60–86400. Default: 300. The minimum is above
    /// the api-gateway request timeout (30 s), so a live upload is never
    /// reaped.
    #[serde(default = "default_upload_reaper_stale_after")]
    pub stale_after_secs: u64,
}

impl Default for UploadReaperConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_interval_secs: default_upload_reaper_scan_interval(),
            stale_after_secs: default_upload_reaper_stale_after(),
        }
    }
}

impl UploadReaperConfig {
    pub(crate) const MIN_STALE_AFTER_SECS: u64 = 60;

    pub fn validate(&self) -> Result<(), String> {
        if !(Self::MIN_STALE_AFTER_SECS..=86_400).contains(&self.stale_after_secs) {
            return Err(format!(
                "upload_reaper.stale_after_secs must be {}-86400, got {}",
                Self::MIN_STALE_AFTER_SECS,
                self.stale_after_secs
            ));
        }
        if !(1..=MAX_SCAN_INTERVAL_SECS).contains(&self.scan_interval_secs) {
            return Err(format!(
                "upload_reaper.scan_interval_secs must be 1-{MAX_SCAN_INTERVAL_SECS}, got {}",
                self.scan_interval_secs
            ));
        }
        Ok(())
    }
}

fn default_upload_reaper_scan_interval() -> u64 {
    60
}
fn default_upload_reaper_stale_after() -> u64 {
    300
}
fn default_orphan_timeout() -> u64 {
    300
}

/// Thread summary tasks. Tasks are enqueued at turn finalization and run by
/// the outbox handler on the `outbox.thread_summary_queue_name` queue; no
/// leader election is involved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummaryWorkerConfig {
    /// Enqueue thread summary tasks at turn finalization. Default: `true`.
    #[serde(default = "super::default_true")]
    pub enabled: bool,
    /// Deprecated: has no effect (see ADR-0010).
    #[serde(default = "default_ts_reconcile_interval")]
    pub reconcile_interval_secs: u64,
    /// Outbox lease for one summary task: the handler is cancelled and the
    /// task redelivered after this. Must be 30-3600s. Default: 300s.
    #[serde(default = "default_ts_claim_timeout")]
    pub claim_timeout_secs: u64,
    /// Attempts per task before it is dead-lettered. Default: 3.
    #[serde(default = "default_ts_max_attempts")]
    pub max_attempts: u32,
    /// Compression threshold: summary triggered when estimated input tokens
    /// reach this percentage of the effective input token budget. Default: 80.
    #[serde(default = "default_compression_threshold")]
    pub compression_threshold_pct: u32,
    /// Model ID from the model catalog for summary generation.
    /// Empty string falls back to `gpt-4.1-mini`. Default: empty.
    #[serde(default)]
    pub summary_model_id: String,
    /// Fallback system prompt when `ModelCatalogEntry.thread_summary_prompt` is empty.
    #[serde(default = "default_summary_system_prompt")]
    pub summary_system_prompt: String,
    /// Maximum characters per message included in the summary prompt.
    /// Messages longer than this are truncated with "..." appended.
    /// 0 = no truncation. Default: 4000.
    #[serde(default = "default_message_content_limit")]
    pub message_content_limit: usize,
}

impl Default for ThreadSummaryWorkerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reconcile_interval_secs: default_ts_reconcile_interval(),
            claim_timeout_secs: default_ts_claim_timeout(),
            max_attempts: default_ts_max_attempts(),
            compression_threshold_pct: default_compression_threshold(),
            summary_model_id: String::new(),
            summary_system_prompt: default_summary_system_prompt(),
            message_content_limit: default_message_content_limit(),
        }
    }
}

impl ThreadSummaryWorkerConfig {
    /// Bounds of `claim_timeout_secs`. The upper bound keeps a hung summary
    /// call from holding the outbox partition lease indefinitely.
    pub const MIN_CLAIM_TIMEOUT_SECS: u64 = 30;
    pub const MAX_CLAIM_TIMEOUT_SECS: u64 = 3600;

    pub fn validate(&self) -> Result<(), String> {
        if !(Self::MIN_CLAIM_TIMEOUT_SECS..=Self::MAX_CLAIM_TIMEOUT_SECS)
            .contains(&self.claim_timeout_secs)
        {
            return Err(format!(
                "thread_summary_worker.claim_timeout_secs must be {}-{}, got {}",
                Self::MIN_CLAIM_TIMEOUT_SECS,
                Self::MAX_CLAIM_TIMEOUT_SECS,
                self.claim_timeout_secs
            ));
        }
        if self.max_attempts == 0 {
            return Err("thread_summary_worker.max_attempts must be > 0".to_owned());
        }
        if self.compression_threshold_pct == 0 || self.compression_threshold_pct > 99 {
            return Err(format!(
                "thread_summary_worker.compression_threshold_pct must be 1-99, got {}",
                self.compression_threshold_pct
            ));
        }
        Ok(())
    }

    /// Deprecated fields set to a non-default value. They have no effect.
    #[must_use]
    pub fn deprecated_fields_set(&self) -> Vec<&'static str> {
        let mut set = Vec::new();
        if self.reconcile_interval_secs != default_ts_reconcile_interval() {
            set.push("thread_summary_worker.reconcile_interval_secs");
        }
        set
    }
}

fn default_ts_reconcile_interval() -> u64 {
    60
}
fn default_ts_claim_timeout() -> u64 {
    300
}
fn default_ts_max_attempts() -> u32 {
    3
}
fn default_compression_threshold() -> u32 {
    80
}
fn default_summary_system_prompt() -> String {
    "You are a conversation summarizer. Given a conversation (and optionally an existing \
     summary), produce a detailed structured summary. Respond with an <analysis> block \
     (your reasoning) followed by a <summary> block (the final summary). Only the \
     <summary> content will be stored. Do not invent information not present in the \
     conversation."
        .to_owned()
}
fn default_message_content_limit() -> usize {
    4000
}

/// Cleanup of provider resources for deleted attachments and chats.
///
/// The work runs as outbox handlers on the `outbox.cleanup_queue_name` and
/// `outbox.chat_cleanup_queue_name` queues; there is no polling worker. Only
/// `max_attempts` is used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupWorkerConfig {
    /// Deprecated: has no effect (see ADR-0010). Cleanup handlers always run.
    #[serde(default = "super::default_true")]
    pub enabled: bool,
    /// Deprecated: has no effect (see ADR-0010).
    #[serde(default = "default_cleanup_poll_interval")]
    pub poll_interval_secs: u64,
    /// Deprecated: has no effect (see ADR-0010).
    #[serde(default = "default_cleanup_reconcile_interval")]
    pub reconcile_interval_secs: u64,
    /// Deprecated: has no effect (see ADR-0010).
    #[serde(default = "default_cleanup_stale_timeout")]
    pub stale_in_progress_timeout_secs: u64,
    /// Deprecated: has no effect (see ADR-0010).
    #[serde(default = "default_cleanup_batch_size")]
    pub batch_size: u32,
    /// Max cleanup attempts per attachment before the outbox message is
    /// dead-lettered. Default: 5.
    #[serde(default = "default_cleanup_max_attempts")]
    pub max_attempts: u32,
}

impl Default for CleanupWorkerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: default_cleanup_poll_interval(),
            reconcile_interval_secs: default_cleanup_reconcile_interval(),
            stale_in_progress_timeout_secs: default_cleanup_stale_timeout(),
            batch_size: default_cleanup_batch_size(),
            max_attempts: default_cleanup_max_attempts(),
        }
    }
}

impl CleanupWorkerConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_attempts == 0 {
            return Err("cleanup_worker.max_attempts must be > 0".to_owned());
        }
        Ok(())
    }

    /// Deprecated fields set to a non-default value. They have no effect.
    #[must_use]
    pub fn deprecated_fields_set(&self) -> Vec<&'static str> {
        let mut set = Vec::new();
        if !self.enabled {
            set.push("cleanup_worker.enabled");
        }
        if self.poll_interval_secs != default_cleanup_poll_interval() {
            set.push("cleanup_worker.poll_interval_secs");
        }
        if self.reconcile_interval_secs != default_cleanup_reconcile_interval() {
            set.push("cleanup_worker.reconcile_interval_secs");
        }
        if self.stale_in_progress_timeout_secs != default_cleanup_stale_timeout() {
            set.push("cleanup_worker.stale_in_progress_timeout_secs");
        }
        if self.batch_size != default_cleanup_batch_size() {
            set.push("cleanup_worker.batch_size");
        }
        set
    }
}

fn default_cleanup_poll_interval() -> u64 {
    60
}
fn default_cleanup_reconcile_interval() -> u64 {
    300
}
fn default_cleanup_stale_timeout() -> u64 {
    900
}
fn default_cleanup_batch_size() -> u32 {
    32
}
fn default_cleanup_max_attempts() -> u32 {
    5
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn default_worker_configs_are_valid() {
        OrphanWatchdogConfig::default().validate().unwrap();
        ThreadSummaryWorkerConfig::default().validate().unwrap();
        CleanupWorkerConfig::default().validate().unwrap();
    }

    #[test]
    fn orphan_watchdog_timeout_bounds() {
        let with = |secs| OrphanWatchdogConfig {
            timeout_secs: secs,
            ..OrphanWatchdogConfig::default()
        };
        for secs in [89, 3601] {
            let err = with(secs).validate().unwrap_err();
            assert!(err.contains("timeout_secs"), "{err}");
        }
        for secs in [90, 3600] {
            with(secs).validate().unwrap();
        }
    }

    #[test]
    fn orphan_watchdog_scan_interval_bounds() {
        let with = |secs| OrphanWatchdogConfig {
            scan_interval_secs: secs,
            ..OrphanWatchdogConfig::default()
        };
        for secs in [0, MAX_SCAN_INTERVAL_SECS + 1] {
            let err = with(secs).validate().unwrap_err();
            assert!(err.contains("scan_interval_secs"), "{err}");
        }
        for secs in [1, MAX_SCAN_INTERVAL_SECS] {
            with(secs).validate().unwrap();
        }
    }

    #[test]
    fn claim_timeout_bounds() {
        let with = |secs| ThreadSummaryWorkerConfig {
            claim_timeout_secs: secs,
            ..ThreadSummaryWorkerConfig::default()
        };
        for secs in [29, 3601] {
            let err = with(secs).validate().unwrap_err();
            assert!(err.contains("claim_timeout_secs"), "{err}");
        }
        for secs in [30, 3600] {
            with(secs).validate().unwrap();
        }
    }

    #[test]
    fn deprecated_fields_do_not_fail_validation_and_are_reported() {
        let cleanup = CleanupWorkerConfig {
            enabled: false,
            poll_interval_secs: 0,
            reconcile_interval_secs: 0,
            stale_in_progress_timeout_secs: 0,
            batch_size: 0,
            ..CleanupWorkerConfig::default()
        };
        cleanup.validate().unwrap();
        assert_eq!(cleanup.deprecated_fields_set().len(), 5);
        assert!(
            CleanupWorkerConfig::default()
                .deprecated_fields_set()
                .is_empty()
        );

        let summary = ThreadSummaryWorkerConfig {
            reconcile_interval_secs: 0,
            ..ThreadSummaryWorkerConfig::default()
        };
        summary.validate().unwrap();
        assert_eq!(
            summary.deprecated_fields_set(),
            ["thread_summary_worker.reconcile_interval_secs"]
        );
    }
}
