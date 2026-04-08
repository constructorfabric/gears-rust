use modkit_macros::domain_model;

/// Output port for recording domain-level metrics.
///
/// Implementations live in `infra/metrics.rs` (OpenTelemetry instruments).
/// Domain services depend only on this trait — no knowledge of `OTel`.
///
/// All 26 P0+P1 metrics are exposed here. Deferred (P2/P3) metrics are
/// declared as `OTel` instruments in the infra layer but NOT on this trait
/// until their feature is ready.
///
/// ## Naming convention
///
/// Instrument names intentionally omit the `_total` suffix that appears in
/// Prometheus metric names. The OpenTelemetry SDK (via
/// `opentelemetry-prometheus`) automatically appends `_total` to all
/// `Counter` instruments during Prometheus export, so including it in the
/// instrument name would produce a doubled `_total_total` suffix.
#[allow(dead_code)]
pub trait MiniChatMetricsPort: Send + Sync {
    // ── P0: Streaming & UX Health (8 metrics) ──────────────────────────
    //
    // ## Stream outcome counter invariant
    //
    // Terminal states are **disjoint**:
    //   `stream_started = stream_completed + stream_failed`
    //
    // `stream_incomplete` is a **diagnostic sub-counter** of `completed`,
    // not a separate terminal state:
    //   `stream_incomplete ⊆ stream_completed`
    //
    // An incomplete stream therefore increments *both*
    // `record_stream_incomplete` and `record_stream_completed`.
    // Dashboard authors: do NOT sum incomplete + completed — incomplete
    // is already included in the completed total.

    /// `{prefix}_stream_started_total` — counter
    fn record_stream_started(&self, provider: &str, model: &str);

    /// `{prefix}_stream_completed_total` — counter
    ///
    /// Covers both `response.completed` and `response.incomplete` outcomes.
    /// See the stream outcome invariant note above.
    fn record_stream_completed(&self, provider: &str, model: &str);

    /// `{prefix}_stream_failed_total` — counter
    fn record_stream_failed(&self, provider: &str, model: &str, error_code: &str);

    /// `{prefix}_stream_disconnected_total` — counter
    /// `stage`: `before_first_token`, `mid_stream`, `after_done`
    fn record_stream_disconnected(&self, stage: &str);

    /// `{prefix}_active_streams` — gauge increment/decrement
    fn increment_active_streams(&self);
    fn decrement_active_streams(&self);

    /// `{prefix}_ttft_provider_ms` — histogram (time-to-first-token from provider)
    fn record_ttft_provider_ms(&self, provider: &str, model: &str, ms: f64);

    /// `{prefix}_ttft_overhead_ms` — histogram (additional overhead beyond provider TTFT)
    fn record_ttft_overhead_ms(&self, provider: &str, model: &str, ms: f64);

    /// `{prefix}_stream_total_latency_ms` — histogram
    fn record_stream_total_latency_ms(&self, provider: &str, model: &str, ms: f64);

    // ── P0: Turn Mutations (2 metrics) ─────────────────────────────────

    /// `{prefix}_turn_mutation_total` — counter
    /// `op`: `retry`, `edit`, `delete`
    /// `result`: `ok`, `not_latest`, `invalid_state`, `forbidden`
    fn record_turn_mutation(&self, op: &str, result: &str);

    /// `{prefix}_turn_mutation_latency_ms` — histogram
    fn record_turn_mutation_latency_ms(&self, op: &str, ms: f64);

    // ── P0: Audit Emission Health (2 metrics) ──────────────────────────

    /// `{prefix}_audit_emit_total` — counter
    /// `result`: `ok`, `retry`, `reject`
    fn record_audit_emit(&self, result: &str);

    /// `{prefix}_finalization_latency_ms` — histogram
    fn record_finalization_latency_ms(&self, ms: f64);

    // ── P1: Quota Enforcement (6 metrics) ──────────────────────────────

    /// `{prefix}_quota_preflight_total` — counter
    fn record_quota_preflight(&self, decision: &str, model: &str, tier: &str);

    /// `{prefix}_quota_reserve_total` — counter
    fn record_quota_reserve(&self, period: &str);

    /// `{prefix}_quota_commit_total` — counter
    fn record_quota_commit(&self, period: &str);

    /// `{prefix}_quota_overshoot_total` — counter
    fn record_quota_overshoot(&self, period: &str);

    /// `{prefix}_quota_estimated_tokens` — histogram
    fn record_quota_estimated_tokens(&self, tokens: f64);

    /// `{prefix}_quota_actual_tokens` — histogram
    fn record_quota_actual_tokens(&self, tokens: f64);

    // ── P1: Streaming Incomplete (1 metric) ────────────────────────────

    /// `{prefix}_stream_incomplete_total` — counter
    ///
    /// Diagnostic sub-counter of `stream_completed`. Always called
    /// **alongside** `record_stream_completed`, never instead of it.
    /// See the stream outcome invariant note in the P0 section above.
    fn record_stream_incomplete(&self, provider: &str, model: &str, reason: &str);

    // ── P1: Cancellation (4 metrics) ───────────────────────────────────

    /// `{prefix}_cancel_requested_total` — counter
    /// `trigger`: `user_stop`, `disconnect`, `timeout`
    fn record_cancel_requested(&self, trigger: &str);

    /// `{prefix}_cancel_effective_total` — counter
    fn record_cancel_effective(&self, trigger: &str);

    /// `{prefix}_time_to_abort_ms` — histogram
    fn record_time_to_abort_ms(&self, trigger: &str, ms: f64);

    /// `{prefix}_streams_aborted_total` — counter
    /// `trigger`: `client_disconnect`, `pod_crash`, `orphan_timeout`, `internal_abort`
    fn record_streams_aborted(&self, trigger: &str);

    // ── P1: Attachment Upload (3 metrics) ──────────────────────────────

    /// `{prefix}_attachment_upload_total` — counter
    /// `kind`: `document`, `image`
    /// `result`: `ok`, `file_too_large`, `unsupported_type`, `provider_error`
    fn record_attachment_upload(&self, kind: &str, result: &str);

    /// `{prefix}_attachment_upload_bytes` — histogram
    fn record_attachment_upload_bytes(&self, kind: &str, bytes: f64);

    /// `{prefix}_attachments_pending` — gauge increment/decrement
    fn increment_attachments_pending(&self);
    fn decrement_attachments_pending(&self);

    /// `{prefix}_image_inputs_per_turn` — histogram
    fn record_image_inputs_per_turn(&self, count: u32);
}

/// No-op implementation for use in tests or when metrics are disabled.
#[allow(dead_code)]
#[domain_model]
pub struct NoopMetrics;

impl MiniChatMetricsPort for NoopMetrics {
    fn record_stream_started(&self, _: &str, _: &str) {}
    fn record_stream_completed(&self, _: &str, _: &str) {}
    fn record_stream_failed(&self, _: &str, _: &str, _: &str) {}
    fn record_stream_disconnected(&self, _: &str) {}
    fn increment_active_streams(&self) {}
    fn decrement_active_streams(&self) {}
    fn record_ttft_provider_ms(&self, _: &str, _: &str, _: f64) {}
    fn record_ttft_overhead_ms(&self, _: &str, _: &str, _: f64) {}
    fn record_stream_total_latency_ms(&self, _: &str, _: &str, _: f64) {}
    fn record_turn_mutation(&self, _: &str, _: &str) {}
    fn record_turn_mutation_latency_ms(&self, _: &str, _: f64) {}
    fn record_audit_emit(&self, _: &str) {}
    fn record_finalization_latency_ms(&self, _: f64) {}
    fn record_quota_preflight(&self, _: &str, _: &str, _: &str) {}
    fn record_quota_reserve(&self, _: &str) {}
    fn record_quota_commit(&self, _: &str) {}
    fn record_quota_overshoot(&self, _: &str) {}
    fn record_quota_estimated_tokens(&self, _: f64) {}
    fn record_quota_actual_tokens(&self, _: f64) {}
    fn record_stream_incomplete(&self, _: &str, _: &str, _: &str) {}
    fn record_cancel_requested(&self, _: &str) {}
    fn record_cancel_effective(&self, _: &str) {}
    fn record_time_to_abort_ms(&self, _: &str, _: f64) {}
    fn record_streams_aborted(&self, _: &str) {}
    fn record_attachment_upload(&self, _: &str, _: &str) {}
    fn record_attachment_upload_bytes(&self, _: &str, _: f64) {}
    fn increment_attachments_pending(&self) {}
    fn decrement_attachments_pending(&self) {}
    fn record_image_inputs_per_turn(&self, _: u32) {}
}
