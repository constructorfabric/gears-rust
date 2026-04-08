use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, UpDownCounter};

use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::ports::metric_labels::key;

/// OpenTelemetry-backed implementation of [`MiniChatMetricsPort`].
///
/// Holds ALL `OTel` instruments. Active (P0+P1) instruments implement the
/// port trait. Deferred instruments are `#[allow(dead_code)]` fields with
/// `// deferred:` comments — they compile but produce no runtime overhead
/// until wired.
///
/// ## `_total` suffix
///
/// Counter instrument names intentionally omit the `_total` suffix from
/// Prometheus metric names. The `opentelemetry-prometheus`
/// exporter appends `_total` automatically for counters, so including it
/// here would produce a doubled `_total_total` suffix.
pub struct MiniChatMetricsMeter {
    // ── P0: Streaming & UX Health ──────────────────────────────────────
    stream_started: Counter<u64>,
    stream_completed: Counter<u64>,
    stream_failed: Counter<u64>,
    stream_disconnected: Counter<u64>,
    active_streams: UpDownCounter<i64>,
    ttft_provider_ms: Histogram<f64>,
    ttft_overhead_ms: Histogram<f64>,
    stream_total_latency_ms: Histogram<f64>,

    // ── P0: Turn Mutations ─────────────────────────────────────────────
    turn_mutation: Counter<u64>,
    turn_mutation_latency_ms: Histogram<f64>,

    // ── P0: Audit Emission Health ──────────────────────────────────────
    audit_emit: Counter<u64>,
    finalization_latency_ms: Histogram<f64>,

    // ── P1: Quota Enforcement ──────────────────────────────────────────
    quota_preflight: Counter<u64>,
    quota_reserve: Counter<u64>,
    quota_commit: Counter<u64>,
    quota_overshoot: Counter<u64>,
    quota_estimated_tokens: Histogram<f64>,
    quota_actual_tokens: Histogram<f64>,

    // ── P1: Streaming Incomplete ───────────────────────────────────────
    stream_incomplete: Counter<u64>,

    // ── P1: Cancellation ───────────────────────────────────────────────
    cancel_requested: Counter<u64>,
    cancel_effective: Counter<u64>,
    time_to_abort_ms: Histogram<f64>,
    streams_aborted: Counter<u64>,

    // ── P1: Attachment Upload ──────────────────────────────────────────
    attachment_upload: Counter<u64>,
    attachment_upload_bytes: Histogram<f64>,
    attachments_pending: UpDownCounter<i64>,

    // ════════════════════════════════════════════════════════════════════
    // DEFERRED INSTRUMENTS — declared but not wired to the domain port.
    // ════════════════════════════════════════════════════════════════════

    // ── P2: Provider / OAGW (deferred: waiting for stable provider endpoint enum) ──
    #[allow(dead_code)]
    provider_requests: Counter<u64>,
    #[allow(dead_code)]
    provider_errors: Counter<u64>,
    #[allow(dead_code)]
    provider_latency_ms: Histogram<f64>,
    #[allow(dead_code)]
    oagw_retries: Counter<u64>,
    #[allow(dead_code)]
    oagw_upstream_latency_ms: Histogram<f64>,
    #[allow(dead_code)]
    oagw_circuit_open: Counter<u64>,

    // ── P2: Outbox (deferred: waiting for modkit_db::outbox integration) ──
    #[allow(dead_code)]
    outbox_dead: Counter<u64>,
    #[allow(dead_code)]
    outbox_pending_age_seconds: Histogram<f64>,
    #[allow(dead_code)]
    outbox_dead_rows: Gauge<i64>,
    #[allow(dead_code)]
    outbox_oldest_pending_age_seconds: Gauge<f64>,
    #[allow(dead_code)]
    outbox_enqueue: Counter<u64>,
    #[allow(dead_code)]
    outbox_dispatch: Counter<u64>,

    // ── P2: Database (deferred: need bounded query allowlist) ──
    #[allow(dead_code)]
    db_query_latency_ms: Histogram<f64>,
    #[allow(dead_code)]
    db_errors: Counter<u64>,

    // ── P2: Cancel deep instrumentation (deferred: complex cancel instrumentation) ──
    #[allow(dead_code)]
    tokens_after_cancel: Histogram<f64>,
    #[allow(dead_code)]
    time_from_ui_disconnect_to_cancel_ms: Histogram<f64>,
    #[allow(dead_code)]
    cancel_orphan: Counter<u64>,

    // ── P3: Tool calls (deferred: tool call execution not implemented) ──
    #[allow(dead_code)]
    tool_calls: Counter<u64>,
    #[allow(dead_code)]
    tool_call_limited: Counter<u64>,
    #[allow(dead_code)]
    file_search_latency_ms: Histogram<f64>,
    #[allow(dead_code)]
    web_search_latency_ms: Histogram<f64>,
    #[allow(dead_code)]
    web_search_disabled: Counter<u64>,
    #[allow(dead_code)]
    citations_count: Histogram<f64>,
    #[allow(dead_code)]
    citations_by_source: Counter<u64>,

    // ── P3: Vector store (deferred: vector store integration not implemented) ──
    #[allow(dead_code)]
    retrieval_latency_ms: Histogram<f64>,
    #[allow(dead_code)]
    retrieval_chunks_returned: Histogram<f64>,
    #[allow(dead_code)]
    retrieval_zero_hit: Counter<u64>,
    #[allow(dead_code)]
    indexed_chunks_per_chat: Histogram<f64>,
    #[allow(dead_code)]
    upload_rejected: Counter<u64>,
    #[allow(dead_code)]
    vector_stores_per_user: Histogram<f64>,
    #[allow(dead_code)]
    context_truncation: Counter<u64>,

    // ── P3: Cleanup job (deferred: cleanup job not implemented) ──
    #[allow(dead_code)]
    cleanup_job_runs: Counter<u64>,
    #[allow(dead_code)]
    cleanup_attempts: Counter<u64>,
    #[allow(dead_code)]
    cleanup_orphan_found: Counter<u64>,
    #[allow(dead_code)]
    cleanup_orphan_fixed: Counter<u64>,
    #[allow(dead_code)]
    cleanup_backlog: Gauge<i64>,
    #[allow(dead_code)]
    cleanup_latency_ms: Histogram<f64>,

    // ── P3: Summary (deferred: summary feature not implemented) ──
    #[allow(dead_code)]
    summary_regen: Counter<u64>,
    #[allow(dead_code)]
    summary_fallback: Counter<u64>,

    // ── P3: Image quota (deferred: image quota not implemented) ──
    #[allow(dead_code)]
    quota_image_commit: Counter<u64>,

    // ── P3: Idempotent replay (deferred: idempotent replay not implemented) ──
    #[allow(dead_code)]
    stream_replay: Counter<u64>,

    // ── P2: Quota v2 (deferred: image quota not implemented) ──
    #[allow(dead_code)]
    quota_preflight_v2: Counter<u64>,
    #[allow(dead_code)]
    quota_overshoot_tokens: Histogram<f64>,
    // quota_reserved_tokens{period} gauge: omitted — only needed if a pending/reserved concept exists

    // ── P2: Attachment indexing (deferred: indexing pipeline not implemented) ──
    #[allow(dead_code)]
    attachment_index: Counter<u64>,
    #[allow(dead_code)]
    attachment_summary: Counter<u64>,
    #[allow(dead_code)]
    attachments_failed: UpDownCounter<i64>,
    #[allow(dead_code)]
    attachment_index_latency_ms: Histogram<f64>,

    // ── P3: Image usage (deferred: image support not implemented) ──
    #[allow(dead_code)]
    image_inputs_per_turn: Histogram<f64>,
    #[allow(dead_code)]
    image_turns: Counter<u64>,

    // ── P2: Orphan watchdog (deferred: watchdog not implemented) ──
    #[allow(dead_code)]
    orphan_turn: Counter<u64>,

    // ── Low-priority deferred ──────────────────────────────────────────
    #[allow(dead_code)]
    quota_tier_downgrade: Counter<u64>, // deferred: tier downgrade logic doesn't exist yet
    #[allow(dead_code)]
    quota_negative: Counter<u64>, // deferred: negative balance alerting
    #[allow(dead_code)]
    audit_redaction_hits: Counter<u64>, // deferred: redaction patterns not configurable
    #[allow(dead_code)]
    media_rejected: Counter<u64>, // deferred: overlaps with attachment_upload
    #[allow(dead_code)]
    unknown_error_code: Counter<u64>, // deferred: cardinality risk
    #[allow(dead_code)]
    credits_overflow: Counter<u64>, // deferred: billing edge case
}

impl MiniChatMetricsMeter {
    /// Create all instruments. `prefix` defaults to `"mini_chat"`.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn new(meter: &Meter, prefix: &str) -> Self {
        Self {
            // ── P0: Streaming ──────────────────────────────────────────
            stream_started: meter
                .u64_counter(format!("{prefix}_stream_started"))
                .with_description("Streams initiated")
                .build(),
            stream_completed: meter
                .u64_counter(format!("{prefix}_stream_completed"))
                .with_description("Streams completed successfully")
                .build(),
            stream_failed: meter
                .u64_counter(format!("{prefix}_stream_failed"))
                .with_description("Streams that ended with an error")
                .build(),
            stream_disconnected: meter
                .u64_counter(format!("{prefix}_stream_disconnected"))
                .with_description("Client disconnects by lifecycle stage")
                .build(),
            active_streams: meter
                .i64_up_down_counter(format!("{prefix}_active_streams"))
                .with_description("Currently active SSE streams")
                .build(),
            ttft_provider_ms: meter
                .f64_histogram(format!("{prefix}_ttft_provider_ms"))
                .with_description("Time-to-first-token from provider (ms)")
                .build(),
            ttft_overhead_ms: meter
                .f64_histogram(format!("{prefix}_ttft_overhead_ms"))
                .with_description("Additional overhead beyond provider TTFT (ms)")
                .build(),
            stream_total_latency_ms: meter
                .f64_histogram(format!("{prefix}_stream_total_latency_ms"))
                .with_description("Total stream duration (ms)")
                .build(),

            // ── P0: Turn Mutations ─────────────────────────────────────
            turn_mutation: meter
                .u64_counter(format!("{prefix}_turn_mutation"))
                .with_description("Turn mutation operations")
                .build(),
            turn_mutation_latency_ms: meter
                .f64_histogram(format!("{prefix}_turn_mutation_latency_ms"))
                .with_description("Turn mutation latency (ms)")
                .build(),

            // ── P0: Audit ──────────────────────────────────────────────
            audit_emit: meter
                .u64_counter(format!("{prefix}_audit_emit"))
                .with_description("Audit event emissions")
                .build(),
            finalization_latency_ms: meter
                .f64_histogram(format!("{prefix}_finalization_latency_ms"))
                .with_description("Turn finalization latency (ms)")
                .build(),

            // ── P1: Quota ──────────────────────────────────────────────
            quota_preflight: meter
                .u64_counter(format!("{prefix}_quota_preflight"))
                .with_description("Quota preflight decisions")
                .build(),
            quota_reserve: meter
                .u64_counter(format!("{prefix}_quota_reserve"))
                .with_description("Quota reserves written")
                .build(),
            quota_commit: meter
                .u64_counter(format!("{prefix}_quota_commit"))
                .with_description("Quota commits (actual usage)")
                .build(),
            quota_overshoot: meter
                .u64_counter(format!("{prefix}_quota_overshoot"))
                .with_description("Quota overshoots (actual > reserved)")
                .build(),
            quota_estimated_tokens: meter
                .f64_histogram(format!("{prefix}_quota_estimated_tokens"))
                .with_description("Estimated token count at preflight")
                .build(),
            quota_actual_tokens: meter
                .f64_histogram(format!("{prefix}_quota_actual_tokens"))
                .with_description("Actual token count at settlement")
                .build(),

            // ── P1: Stream Incomplete ──────────────────────────────────
            stream_incomplete: meter
                .u64_counter(format!("{prefix}_stream_incomplete"))
                .with_description("Streams that ended incomplete (e.g. max_output_tokens)")
                .build(),

            // ── P1: Cancellation ───────────────────────────────────────
            cancel_requested: meter
                .u64_counter(format!("{prefix}_cancel_requested"))
                .with_description("Cancel requests received")
                .build(),
            cancel_effective: meter
                .u64_counter(format!("{prefix}_cancel_effective"))
                .with_description("Cancels that took effect")
                .build(),
            time_to_abort_ms: meter
                .f64_histogram(format!("{prefix}_time_to_abort_ms"))
                .with_description("Time from cancel request to stream abort (ms)")
                .build(),
            streams_aborted: meter
                .u64_counter(format!("{prefix}_streams_aborted"))
                .with_description("Streams aborted by trigger type")
                .build(),

            // ── P1: Attachment ─────────────────────────────────────────
            attachment_upload: meter
                .u64_counter(format!("{prefix}_attachment_upload"))
                .with_description("Attachment upload attempts")
                .build(),
            attachment_upload_bytes: meter
                .f64_histogram(format!("{prefix}_attachment_upload_bytes"))
                .with_description("Attachment upload size (bytes)")
                .build(),
            attachments_pending: meter
                .i64_up_down_counter(format!("{prefix}_attachments_pending"))
                .with_description("Attachments currently being processed")
                .build(),

            // ════════════════════════════════════════════════════════════
            // DEFERRED INSTRUMENTS
            // ════════════════════════════════════════════════════════════

            // deferred: waiting for stable provider endpoint enum
            provider_requests: meter
                .u64_counter(format!("{prefix}_provider_requests"))
                .with_description("Provider API requests")
                .build(),
            provider_errors: meter
                .u64_counter(format!("{prefix}_provider_errors"))
                .with_description("Provider API errors")
                .build(),
            provider_latency_ms: meter
                .f64_histogram(format!("{prefix}_provider_latency_ms"))
                .with_description("Provider API latency (ms)")
                .build(),
            oagw_retries: meter
                .u64_counter(format!("{prefix}_oagw_retries"))
                .with_description("OAGW retry attempts")
                .build(),
            oagw_upstream_latency_ms: meter
                .f64_histogram(format!("{prefix}_oagw_upstream_latency_ms"))
                .with_description("OAGW upstream latency (ms)")
                .build(),
            oagw_circuit_open: meter
                .u64_counter(format!("{prefix}_oagw_circuit_open"))
                .with_description("OAGW circuit breaker opens")
                .build(),

            // deferred: waiting for modkit_db::outbox integration
            outbox_dead: meter
                .u64_counter(format!("{prefix}_outbox_dead"))
                .with_description("Outbox dead letter events")
                .build(),
            outbox_pending_age_seconds: meter
                .f64_histogram(format!("{prefix}_outbox_pending_age_seconds"))
                .with_description("Outbox pending event age (seconds)")
                .build(),
            outbox_dead_rows: meter
                .i64_gauge(format!("{prefix}_outbox_dead_rows"))
                .with_description("Current dead letter row count")
                .build(),
            outbox_oldest_pending_age_seconds: meter
                .f64_gauge(format!("{prefix}_outbox_oldest_pending_age_seconds"))
                .with_description("Age of oldest pending outbox event (seconds)")
                .build(),
            outbox_enqueue: meter
                .u64_counter(format!("{prefix}_outbox_enqueue"))
                .with_description("Outbox events enqueued")
                .build(),
            outbox_dispatch: meter
                .u64_counter(format!("{prefix}_outbox_dispatch"))
                .with_description("Outbox events dispatched")
                .build(),

            // deferred: need bounded query allowlist
            db_query_latency_ms: meter
                .f64_histogram(format!("{prefix}_db_query_latency_ms"))
                .with_description("DB query latency (ms)")
                .build(),
            db_errors: meter
                .u64_counter(format!("{prefix}_db_errors"))
                .with_description("DB errors")
                .build(),

            // deferred: complex cancel instrumentation
            tokens_after_cancel: meter
                .f64_histogram(format!("{prefix}_tokens_after_cancel"))
                .with_description("Tokens received after cancel signal")
                .build(),
            time_from_ui_disconnect_to_cancel_ms: meter
                .f64_histogram(format!("{prefix}_time_from_ui_disconnect_to_cancel_ms"))
                .with_description("Time from UI disconnect to effective cancel (ms)")
                .build(),
            cancel_orphan: meter
                .u64_counter(format!("{prefix}_cancel_orphan"))
                .with_description("Orphaned cancel events")
                .build(),

            // deferred: tool call execution not implemented
            tool_calls: meter
                .u64_counter(format!("{prefix}_tool_calls"))
                .with_description("Tool call invocations")
                .build(),
            tool_call_limited: meter
                .u64_counter(format!("{prefix}_tool_call_limited"))
                .with_description("Tool calls rejected by limit")
                .build(),
            file_search_latency_ms: meter
                .f64_histogram(format!("{prefix}_file_search_latency_ms"))
                .with_description("File search tool latency (ms)")
                .build(),
            web_search_latency_ms: meter
                .f64_histogram(format!("{prefix}_web_search_latency_ms"))
                .with_description("Web search tool latency (ms)")
                .build(),
            web_search_disabled: meter
                .u64_counter(format!("{prefix}_web_search_disabled"))
                .with_description("Web search requests rejected (disabled)")
                .build(),
            citations_count: meter
                .f64_histogram(format!("{prefix}_citations_count"))
                .with_description("Citations per response")
                .build(),
            citations_by_source: meter
                .u64_counter(format!("{prefix}_citations_by_source"))
                .with_description("Citations by source type")
                .build(),

            // deferred: vector store integration not implemented
            retrieval_latency_ms: meter
                .f64_histogram(format!("{prefix}_retrieval_latency_ms"))
                .with_description("Vector retrieval latency (ms)")
                .build(),
            retrieval_chunks_returned: meter
                .f64_histogram(format!("{prefix}_retrieval_chunks_returned"))
                .with_description("Chunks returned per retrieval")
                .build(),
            retrieval_zero_hit: meter
                .u64_counter(format!("{prefix}_retrieval_zero_hit"))
                .with_description("Retrievals returning zero results")
                .build(),
            indexed_chunks_per_chat: meter
                .f64_histogram(format!("{prefix}_indexed_chunks_per_chat"))
                .with_description("Indexed chunks per chat")
                .build(),
            upload_rejected: meter
                .u64_counter(format!("{prefix}_upload_rejected"))
                .with_description("File uploads rejected")
                .build(),
            vector_stores_per_user: meter
                .f64_histogram(format!("{prefix}_vector_stores_per_user"))
                .with_description("Vector stores per user")
                .build(),
            context_truncation: meter
                .u64_counter(format!("{prefix}_context_truncation"))
                .with_description("Context truncation events")
                .build(),

            // deferred: cleanup job not implemented
            cleanup_job_runs: meter
                .u64_counter(format!("{prefix}_cleanup_job_runs"))
                .with_description("Cleanup job runs")
                .build(),
            cleanup_attempts: meter
                .u64_counter(format!("{prefix}_cleanup_attempts"))
                .with_description("Cleanup attempts")
                .build(),
            cleanup_orphan_found: meter
                .u64_counter(format!("{prefix}_cleanup_orphan_found"))
                .with_description("Orphaned resources found during cleanup")
                .build(),
            cleanup_orphan_fixed: meter
                .u64_counter(format!("{prefix}_cleanup_orphan_fixed"))
                .with_description("Orphaned resources fixed during cleanup")
                .build(),
            cleanup_backlog: meter
                .i64_gauge(format!("{prefix}_cleanup_backlog"))
                .with_description("Current cleanup backlog")
                .build(),
            cleanup_latency_ms: meter
                .f64_histogram(format!("{prefix}_cleanup_latency_ms"))
                .with_description("Cleanup operation latency (ms)")
                .build(),

            // deferred: summary feature not implemented
            summary_regen: meter
                .u64_counter(format!("{prefix}_summary_regen"))
                .with_description("Summary regeneration events")
                .build(),
            summary_fallback: meter
                .u64_counter(format!("{prefix}_summary_fallback"))
                .with_description("Summary fallback events")
                .build(),

            // deferred: image quota not implemented
            quota_image_commit: meter
                .u64_counter(format!("{prefix}_quota_image_commit"))
                .with_description("Image quota commits")
                .build(),

            // deferred: idempotent replay not implemented
            stream_replay: meter
                .u64_counter(format!("{prefix}_stream_replay"))
                .with_description("Stream replay events")
                .build(),

            // deferred: image quota not implemented (v2 with kind dimension)
            quota_preflight_v2: meter
                .u64_counter(format!("{prefix}_quota_preflight_v2"))
                .with_description("Quota preflight decisions (v2, with kind label)")
                .build(),
            quota_overshoot_tokens: meter
                .f64_histogram(format!("{prefix}_quota_overshoot_tokens"))
                .with_description("Overshoot token count: max(actual - estimate, 0)")
                .build(),

            // deferred: indexing pipeline not implemented
            attachment_index: meter
                .u64_counter(format!("{prefix}_attachment_index"))
                .with_description("Attachment indexing results")
                .build(),
            attachment_summary: meter
                .u64_counter(format!("{prefix}_attachment_summary"))
                .with_description("Attachment summary generation results")
                .build(),
            attachments_failed: meter
                .i64_up_down_counter(format!("{prefix}_attachments_failed"))
                .with_description("Attachments currently in failed state")
                .build(),
            attachment_index_latency_ms: meter
                .f64_histogram(format!("{prefix}_attachment_index_latency_ms"))
                .with_description("Attachment indexing latency (ms)")
                .build(),

            // deferred: image support not implemented
            image_inputs_per_turn: meter
                .f64_histogram(format!("{prefix}_image_inputs_per_turn"))
                .with_description("Images included per Responses API call")
                .build(),
            image_turns: meter
                .u64_counter(format!("{prefix}_image_turns"))
                .with_description("Turns that included >=1 image")
                .build(),

            // deferred: orphan watchdog not implemented
            orphan_turn: meter
                .u64_counter(format!("{prefix}_orphan_turn"))
                .with_description("Orphan turns detected by watchdog")
                .build(),

            // deferred: low-priority
            quota_tier_downgrade: meter
                .u64_counter(format!("{prefix}_quota_tier_downgrade"))
                .with_description("Quota tier downgrades")
                .build(),
            quota_negative: meter
                .u64_counter(format!("{prefix}_quota_negative"))
                .with_description("Negative quota balance events")
                .build(),
            audit_redaction_hits: meter
                .u64_counter(format!("{prefix}_audit_redaction_hits"))
                .with_description("Audit redaction hits")
                .build(),
            media_rejected: meter
                .u64_counter(format!("{prefix}_media_rejected"))
                .with_description("Media files rejected")
                .build(),
            unknown_error_code: meter
                .u64_counter(format!("{prefix}_unknown_error_code"))
                .with_description("Unknown error codes encountered")
                .build(),
            credits_overflow: meter
                .u64_counter(format!("{prefix}_credits_overflow"))
                .with_description("Credits overflow events")
                .build(),
        }
    }
}

impl MiniChatMetricsPort for MiniChatMetricsMeter {
    // ── P0: Streaming ──────────────────────────────────────────────────

    fn record_stream_started(&self, provider: &str, model: &str) {
        self.stream_started.add(
            1,
            &[
                KeyValue::new(key::PROVIDER, provider.to_owned()),
                KeyValue::new(key::MODEL, model.to_owned()),
            ],
        );
    }

    fn record_stream_completed(&self, provider: &str, model: &str) {
        self.stream_completed.add(
            1,
            &[
                KeyValue::new(key::PROVIDER, provider.to_owned()),
                KeyValue::new(key::MODEL, model.to_owned()),
            ],
        );
    }

    fn record_stream_failed(&self, provider: &str, model: &str, error_code: &str) {
        self.stream_failed.add(
            1,
            &[
                KeyValue::new(key::PROVIDER, provider.to_owned()),
                KeyValue::new(key::MODEL, model.to_owned()),
                KeyValue::new(key::ERROR_CODE, error_code.to_owned()),
            ],
        );
    }

    fn record_stream_disconnected(&self, stage: &str) {
        self.stream_disconnected
            .add(1, &[KeyValue::new(key::STAGE, stage.to_owned())]);
    }

    fn increment_active_streams(&self) {
        self.active_streams.add(1, &[]);
    }

    fn decrement_active_streams(&self) {
        self.active_streams.add(-1, &[]);
    }

    fn record_ttft_provider_ms(&self, provider: &str, model: &str, ms: f64) {
        self.ttft_provider_ms.record(
            ms,
            &[
                KeyValue::new(key::PROVIDER, provider.to_owned()),
                KeyValue::new(key::MODEL, model.to_owned()),
            ],
        );
    }

    fn record_ttft_overhead_ms(&self, provider: &str, model: &str, ms: f64) {
        self.ttft_overhead_ms.record(
            ms,
            &[
                KeyValue::new(key::PROVIDER, provider.to_owned()),
                KeyValue::new(key::MODEL, model.to_owned()),
            ],
        );
    }

    fn record_stream_total_latency_ms(&self, provider: &str, model: &str, ms: f64) {
        self.stream_total_latency_ms.record(
            ms,
            &[
                KeyValue::new(key::PROVIDER, provider.to_owned()),
                KeyValue::new(key::MODEL, model.to_owned()),
            ],
        );
    }

    // ── P0: Turn Mutations ─────────────────────────────────────────────

    fn record_turn_mutation(&self, op: &str, result: &str) {
        self.turn_mutation.add(
            1,
            &[
                KeyValue::new(key::OP, op.to_owned()),
                KeyValue::new(key::RESULT, result.to_owned()),
            ],
        );
    }

    fn record_turn_mutation_latency_ms(&self, op: &str, ms: f64) {
        self.turn_mutation_latency_ms
            .record(ms, &[KeyValue::new(key::OP, op.to_owned())]);
    }

    // ── P0: Audit ──────────────────────────────────────────────────────

    fn record_audit_emit(&self, result: &str) {
        self.audit_emit
            .add(1, &[KeyValue::new(key::RESULT, result.to_owned())]);
    }

    fn record_finalization_latency_ms(&self, ms: f64) {
        self.finalization_latency_ms.record(ms, &[]);
    }

    // ── P1: Quota ──────────────────────────────────────────────────────

    fn record_quota_preflight(&self, decision: &str, model: &str, tier: &str) {
        self.quota_preflight.add(
            1,
            &[
                KeyValue::new(key::DECISION, decision.to_owned()),
                KeyValue::new(key::MODEL, model.to_owned()),
                KeyValue::new(key::TIER, tier.to_owned()),
            ],
        );
    }

    fn record_quota_reserve(&self, period: &str) {
        self.quota_reserve
            .add(1, &[KeyValue::new(key::PERIOD, period.to_owned())]);
    }

    fn record_quota_commit(&self, period: &str) {
        self.quota_commit
            .add(1, &[KeyValue::new(key::PERIOD, period.to_owned())]);
    }

    fn record_quota_overshoot(&self, period: &str) {
        self.quota_overshoot
            .add(1, &[KeyValue::new(key::PERIOD, period.to_owned())]);
    }

    fn record_quota_estimated_tokens(&self, tokens: f64) {
        self.quota_estimated_tokens.record(tokens, &[]);
    }

    fn record_quota_actual_tokens(&self, tokens: f64) {
        self.quota_actual_tokens.record(tokens, &[]);
    }

    // ── P1: Stream Incomplete ──────────────────────────────────────────

    fn record_stream_incomplete(&self, provider: &str, model: &str, reason: &str) {
        self.stream_incomplete.add(
            1,
            &[
                KeyValue::new(key::PROVIDER, provider.to_owned()),
                KeyValue::new(key::MODEL, model.to_owned()),
                KeyValue::new(key::REASON, reason.to_owned()),
            ],
        );
    }

    // ── P1: Cancellation ───────────────────────────────────────────────

    fn record_cancel_requested(&self, trigger: &str) {
        self.cancel_requested
            .add(1, &[KeyValue::new(key::TRIGGER, trigger.to_owned())]);
    }

    fn record_cancel_effective(&self, trigger: &str) {
        self.cancel_effective
            .add(1, &[KeyValue::new(key::TRIGGER, trigger.to_owned())]);
    }

    fn record_time_to_abort_ms(&self, trigger: &str, ms: f64) {
        self.time_to_abort_ms
            .record(ms, &[KeyValue::new(key::TRIGGER, trigger.to_owned())]);
    }

    fn record_streams_aborted(&self, trigger: &str) {
        self.streams_aborted
            .add(1, &[KeyValue::new(key::TRIGGER, trigger.to_owned())]);
    }

    // ── P1: Attachment ─────────────────────────────────────────────────

    fn record_attachment_upload(&self, kind: &str, result: &str) {
        self.attachment_upload.add(
            1,
            &[
                KeyValue::new(key::KIND, kind.to_owned()),
                KeyValue::new(key::RESULT, result.to_owned()),
            ],
        );
    }

    fn record_attachment_upload_bytes(&self, kind: &str, bytes: f64) {
        self.attachment_upload_bytes
            .record(bytes, &[KeyValue::new(key::KIND, kind.to_owned())]);
    }

    fn increment_attachments_pending(&self) {
        self.attachments_pending.add(1, &[]);
    }

    fn decrement_attachments_pending(&self) {
        self.attachments_pending.add(-1, &[]);
    }

    fn record_image_inputs_per_turn(&self, count: u32) {
        self.image_inputs_per_turn.record(f64::from(count), &[]);
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{
        InMemoryMetricExporter, Instrument, PeriodicReader, SdkMeterProvider, Stream,
    };

    use crate::domain::ports::MiniChatMetricsPort;

    const CARDINALITY_LIMIT: usize = 2000;

    fn local_provider() -> (SdkMeterProvider, InMemoryMetricExporter) {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .with_view(|_: &Instrument| {
                Stream::builder()
                    .with_cardinality_limit(CARDINALITY_LIMIT)
                    .build()
                    .ok()
            })
            .build();
        (provider, exporter)
    }

    fn extract_counter_value(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
        let metrics = exporter.get_finished_metrics().unwrap();
        for resource_metrics in &metrics {
            for scope_metrics in resource_metrics.scope_metrics() {
                for metric in scope_metrics.metrics() {
                    if metric.name() == name
                        && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                    {
                        return sum
                            .data_points()
                            .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                            .sum();
                    }
                }
            }
        }
        0
    }

    #[test]
    fn stream_counters_increment() {
        let (provider, exporter) = local_provider();
        let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

        m.record_stream_started("openai", "gpt-5.2");
        m.record_stream_started("openai", "gpt-5.2");
        m.record_stream_completed("openai", "gpt-5.2");
        m.record_stream_failed("openai", "gpt-5.2", "rate_limited");

        provider.force_flush().unwrap();

        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_stream_started"),
            2
        );
        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_stream_completed"),
            1
        );
        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_stream_failed"),
            1
        );
    }

    #[test]
    fn turn_mutation_counter_increments() {
        let (provider, exporter) = local_provider();
        let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

        m.record_turn_mutation("retry", "ok");
        m.record_turn_mutation("edit", "not_latest");
        m.record_turn_mutation("delete", "ok");

        provider.force_flush().unwrap();

        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_turn_mutation"),
            3
        );
    }

    #[test]
    fn quota_counters_increment() {
        let (provider, exporter) = local_provider();
        let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

        m.record_quota_preflight("allow", "gpt-5.2", "premium");
        m.record_quota_reserve("daily");
        m.record_quota_commit("daily");
        m.record_quota_overshoot("monthly");

        provider.force_flush().unwrap();

        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_quota_preflight"),
            1
        );
        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_quota_reserve"),
            1
        );
        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_quota_commit"),
            1
        );
        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_quota_overshoot"),
            1
        );
    }

    #[test]
    fn configurable_prefix() {
        let (provider, exporter) = local_provider();
        let m = super::MiniChatMetricsMeter::new(&provider.meter("custom"), "my_chat");

        m.record_stream_started("azure", "gpt-4o");

        provider.force_flush().unwrap();

        assert_eq!(
            extract_counter_value(&exporter, "my_chat_stream_started"),
            1
        );
        // Original prefix should NOT exist
        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_stream_started"),
            0
        );
    }

    #[test]
    fn cancellation_counters_increment() {
        let (provider, exporter) = local_provider();
        let m = super::MiniChatMetricsMeter::new(&provider.meter("mini-chat"), "mini_chat");

        m.record_cancel_requested("user_stop");
        m.record_cancel_effective("user_stop");
        m.record_streams_aborted("client_disconnect");

        provider.force_flush().unwrap();

        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_cancel_requested"),
            1
        );
        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_cancel_effective"),
            1
        );
        assert_eq!(
            extract_counter_value(&exporter, "mini_chat_streams_aborted"),
            1
        );
    }
}
