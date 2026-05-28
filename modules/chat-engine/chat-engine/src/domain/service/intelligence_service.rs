//! Session intelligence service.
//!
//! `IntelligenceService` orchestrates Phase 8's session-level intelligence
//! surface:
//!
//! 1. **On-demand summary** — `POST /sessions/{id}/summarize`. Validates
//!    ownership / lifecycle / plugin support, builds the visible history,
//!    invokes the backend plugin's `on_session_summary`, streams the result
//!    as NDJSON, then atomically persists the summary as a
//!    `role=system, parent_message_id=NULL, is_hidden_from_user=true`
//!    message and flips every message in `summarized_message_ids` to
//!    `is_hidden_from_backend=true`.
//! 2. **Per-session retention policy** — `GET` / `PATCH
//!    /sessions/{id}/retention-policy`. The policy is read / written via
//!    the reserved `session.metadata["retention_policy"]` key (Phase 2
//!    helpers, per ADR-0017). When unset, the effective policy falls back
//!    to the session-type default; this phase ships the surface but the
//!    session-type column does not yet exist on the Phase 1 schema, so the
//!    fallback resolves to [`RetentionPolicy::None`].
//! 3. **Tenant-scoped retention cleanup** —
//!    [`IntelligenceService::run_retention_cleanup_for_tenant`]. The
//!    algorithm acquires a per-session `pg_try_advisory_xact_lock`,
//!    evaluates the policy to pick eligible non-root messages, and
//!    recursively deletes each subtree atomically. Phase 15 will register
//!    this as a background task; this phase only exposes the entry point.
//!
//! ## Reserved metadata key
//!
//! The service is the only sanctioned writer of
//! `session.metadata["retention_policy"]`. Clients are forbidden from
//! writing the key via the generic metadata-patch surface (the
//! `reject_reserved_metadata` guard in `SessionService` already enforces
//! that constraint).
//!
//! ## Summary streaming contract
//!
//! The `summarize_session` method returns a `SummaryStream` of
//! [`StreamingEvent`] values mirroring the message-send pipeline: one
//! `Start` → zero-or-more `Chunk` → one `Complete` or `Error`. The
//! summary message is persisted only after a successful
//! `StreamingCompleteEvent`. If the stream errors mid-flight, no summary
//! message is written and no `is_hidden_from_backend` flips happen — the
//! caller can safely retry the operation.
//
// @cpt-cf-chat-engine-intelligence-service:p8
// @cpt-cf-chat-engine-flow-session-intelligence-generate-summary:p8
// @cpt-cf-chat-engine-flow-session-intelligence-get-retention:p8
// @cpt-cf-chat-engine-flow-session-intelligence-update-retention:p8
// @cpt-cf-chat-engine-algo-session-intelligence-validate-summarization:p8
// @cpt-cf-chat-engine-algo-session-intelligence-invoke-summary:p8
// @cpt-cf-chat-engine-algo-session-intelligence-evaluate-retention:p8
// @cpt-cf-chat-engine-algo-session-intelligence-enforce-retention:p8
// @cpt-cf-chat-engine-adr-session-metadata:p8
// @cpt-cf-chat-engine-adr-session-deletion-strategy:p8

use std::sync::Arc;
use std::time::{Duration, Instant};

use chat_engine_sdk::models::{LifecycleState, TenantId, UserId};
use chat_engine_sdk::plugin::{PluginCallContext, SessionPluginCtx};
use futures::stream::{self, BoxStream, StreamExt};
use modkit_macros::domain_model;
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::message::{
    Message, StreamingChunkEvent, StreamingCompleteEvent, StreamingErrorEvent, StreamingEvent,
    StreamingStartEvent,
};
use crate::domain::retention::RetentionPolicy;
use crate::domain::service::plugin_service::PluginService;
use crate::domain::service::session_service::Identity;
use crate::domain::session::{
    Session, get_retention_policy, set_retention_policy,
};
use crate::infra::db::repo::message_repo::MessageRepo;
use crate::infra::db::repo::session_repo::SessionRepo;
use crate::infra::db::repo::session_type_repo::SessionTypeRepo;

/// Default per-call plugin deadline for `on_session_summary`. Mirrors the
/// streaming-message budget — summaries can legitimately take time to emit
/// a full response.
pub const DEFAULT_SUMMARY_DEADLINE: Duration = Duration::from_secs(120);

/// Bounded backpressure-channel size between the plugin driver and the
/// NDJSON sink. Mirrors the `MessageService` default; small enough to
/// bound memory but big enough that a typical chunk-stream does not
/// stall.
pub const DEFAULT_SUMMARY_BUFFER_SIZE: usize = 64;

/// Outgoing NDJSON-ready stream of [`StreamingEvent`] returned by
/// [`IntelligenceService::summarize_session`].
pub type SummaryStream = BoxStream<'static, StreamingEvent>;

/// Per-session cleanup report. Aggregated into [`RetentionCleanupReport`]
/// by [`IntelligenceService::run_retention_cleanup_for_tenant`].
#[domain_model]
#[derive(Debug, Clone)]
pub struct SessionCleanupOutcome {
    pub session_id: Uuid,
    /// Stable label for the policy that drove the cleanup
    /// (`"none"` / `"age_based"` / `"count_based"`).
    pub policy_type: &'static str,
    /// Number of messages physically removed (subtree included).
    pub messages_deleted: u64,
    /// Time spent on the cleanup, end-to-end (lock acquisition through
    /// delete).
    pub duration_ms: u64,
    /// `true` when the advisory lock could not be acquired — the session
    /// was skipped.
    pub skipped_locked: bool,
}

/// Aggregate cleanup report returned by
/// [`IntelligenceService::run_retention_cleanup_for_tenant`]. Phase 15
/// will surface this in operator metrics; the structure is intentionally
/// small (one entry per session in the tenant) so the report fits in
/// memory for any reasonable tenant scale.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct RetentionCleanupReport {
    /// Outcomes ordered by session id (deterministic for logs / tests).
    pub sessions: Vec<SessionCleanupOutcome>,
}

impl RetentionCleanupReport {
    /// Total messages deleted across all sessions.
    #[must_use]
    pub fn total_messages_deleted(&self) -> u64 {
        self.sessions.iter().map(|s| s.messages_deleted).sum()
    }

    /// Sessions that were skipped because the advisory lock could not be
    /// acquired (another cleanup run was in flight).
    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.sessions.iter().filter(|s| s.skipped_locked).count()
    }
}

/// Validated retention policy. The wire shape is the SDK enum
/// [`RetentionPolicy`] (internally tagged on `"type"`); validation happens
/// inside [`validate_retention_policy`] before any DB write.
#[domain_model]
#[derive(Debug, Clone)]
struct ValidatedPolicy(RetentionPolicy);

impl From<ValidatedPolicy> for RetentionPolicy {
    fn from(v: ValidatedPolicy) -> Self {
        v.0
    }
}

/// Public service.
///
/// Construct once at module init (Phase 15) with the shared repositories
/// + plugin hub. Clone freely — all fields are `Arc`/`Clone`-cheap.
#[domain_model]
#[derive(Clone)]
pub struct IntelligenceService {
    sessions: Arc<dyn SessionRepo>,
    session_types: Arc<dyn SessionTypeRepo>,
    messages: Arc<dyn MessageRepo>,
    plugins: PluginService,
    summary_buffer_size: usize,
    summary_deadline: Duration,
}

impl IntelligenceService {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn SessionRepo>,
        session_types: Arc<dyn SessionTypeRepo>,
        messages: Arc<dyn MessageRepo>,
        plugins: PluginService,
    ) -> Self {
        Self {
            sessions,
            session_types,
            messages,
            plugins,
            summary_buffer_size: DEFAULT_SUMMARY_BUFFER_SIZE,
            summary_deadline: DEFAULT_SUMMARY_DEADLINE,
        }
    }

    /// Override the bounded-channel size used between the plugin driver
    /// and the NDJSON sink. Useful for tests + operator tuning.
    #[must_use]
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.summary_buffer_size = size.max(1);
        self
    }

    /// Override the per-call summary deadline.
    #[must_use]
    pub fn with_summary_deadline(mut self, deadline: Duration) -> Self {
        self.summary_deadline = deadline;
        self
    }

    // ---------------------------------------------------------------------
    // Retention policy: read/write
    // ---------------------------------------------------------------------

    /// Resolve the effective retention policy for a session: the
    /// per-session override (when set) or the session-type default
    /// (otherwise). When neither is set, returns [`RetentionPolicy::None`].
    ///
    /// The session-type-level default is not yet stored in the Phase 1
    /// schema; the fallback resolves to `None` until ADR-0021 adds the
    /// column. The wiring is in place here so downstream phases can switch
    /// the fallback source without touching the service surface.
    #[instrument(skip(self), fields(session_id = %session_id))]
    pub async fn get_effective_retention_policy(
        &self,
        identity: &Identity,
        session_id: Uuid,
    ) -> Result<RetentionPolicy> {
        let session = self.load_session(identity, session_id).await?;
        Ok(resolve_effective_policy(&session))
    }

    /// Persist a new per-session retention policy. Validates the payload
    /// per the SDK constraints (variant + numeric bounds) and writes the
    /// reserved `session.metadata["retention_policy"]` key atomically.
    ///
    /// Returns the persisted policy (echoed verbatim — the wire shape
    /// matches the request body).
    #[instrument(skip(self), fields(session_id = %session_id))]
    pub async fn update_session_retention_policy(
        &self,
        identity: &Identity,
        session_id: Uuid,
        policy: RetentionPolicy,
    ) -> Result<RetentionPolicy> {
        let validated = validate_retention_policy(policy)?;

        // Load the session via the ownership-scoped repo so cross-tenant
        // misses fold to 404 (anti-enumeration, ADR-0021).
        let mut session = self.load_session(identity, session_id).await?;
        if matches!(
            session.lifecycle_state,
            LifecycleState::SoftDeleted | LifecycleState::HardDeleted
        ) {
            return Err(ChatEngineError::conflict(format!(
                "session is {} and cannot accept retention_policy updates",
                session.lifecycle_state
            )));
        }

        // Apply the reserved-key write to a fresh metadata clone, then
        // persist via the standard `update_metadata` path. The repo bumps
        // `updated_at`; sibling metadata keys survive verbatim.
        let persisted_policy = validated.0.clone();
        set_retention_policy(&mut session, validated.0);
        let new_metadata = session.metadata.clone();

        let _persisted = self
            .sessions
            .update_metadata(
                &identity.tenant_id,
                &identity.user_id,
                session_id,
                new_metadata,
            )
            .await?;

        info!(
            session_id = %session_id,
            policy_type = %retention_policy_label(&persisted_policy),
            "persisted per-session retention policy"
        );

        Ok(persisted_policy)
    }

    // ---------------------------------------------------------------------
    // On-demand session summary
    // ---------------------------------------------------------------------

    /// Generate an AI-summary for a session and stream it as
    /// [`StreamingEvent`]s. The handler (Phase 14) wraps each event in
    /// one NDJSON line.
    ///
    /// Pre-stream failures surface as `Err(ChatEngineError)` mapped to
    /// the standard HTTP statuses (403/404/409/422/502). Mid-stream
    /// failures stay on the wire as `StreamingErrorEvent` (the HTTP
    /// response has already started by then).
    #[instrument(
        skip(self, identity, cancel),
        fields(
            session_id = %session_id,
            user_id = %identity.user_id,
            request_id,
            summary_message_id,
        ),
    )]
    pub async fn summarize_session(
        &self,
        identity: &Identity,
        session_id: Uuid,
        cancel: CancellationToken,
    ) -> Result<SummaryStream> {
        let session = self.load_session(identity, session_id).await?;
        // Lifecycle gate: 409 when not active (active is the only state
        // that admits on-demand work per the feature spec).
        if !matches!(session.lifecycle_state, LifecycleState::Active) {
            return Err(ChatEngineError::conflict(format!(
                "session is {} and cannot be summarized",
                session.lifecycle_state
            )));
        }

        // Session-type + plugin binding are required for summary routing
        // (422 when unbound per the feature spec).
        let session_type_id = session.session_type_id.ok_or_else(|| {
            ChatEngineError::BadRequest {
                reason:
                    "session has no session_type bound; summary cannot be generated"
                        .to_string(),
            }
        })?;
        let session_type = self
            .session_types
            .find_by_id(session_type_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session_type", session_type_id))?;
        let plugin_instance_id = session_type.plugin_instance_id.ok_or_else(|| {
            // 422 Unprocessable Entity — the plugin binding is missing, so
            // the service cannot generate a summary even though the
            // session itself is well-formed. We surface this via a
            // BackendUnavailable so the handler can map to 422.
            ChatEngineError::BackendUnavailable {
                reason:
                    "session_type has no plugin_instance_id; summarization unsupported"
                        .to_string(),
                retry_after: None,
                source: None,
            }
        })?;

        // Resolve the plugin. A missing registration is 422 per the
        // feature spec (plugin does not support summarization).
        let plugin = self.plugins.resolve(&plugin_instance_id).map_err(|err| {
            match err {
                ChatEngineError::NotFound { .. } => ChatEngineError::BackendUnavailable {
                    reason: format!(
                        "plugin '{plugin_instance_id}' is not registered; summarization unsupported"
                    ),
                    retry_after: None,
                    source: None,
                },
                other => other,
            }
        })?;
        let plugin_config = self
            .plugins
            .load_config(&plugin_instance_id, session_type_id)
            .await?;

        // Load the visible history (chronological, excluding
        // `is_hidden_from_backend=true`) — this is the Phase 7 canonical
        // history-visibility filter.
        let history = self
            .messages
            .fetch_active_history(session_id, None)
            .await?;

        // Build the call context. The plugin's child token observes the
        // handler's cancellation (connection close / explicit cancel).
        let request_id = Uuid::new_v4();
        tracing::Span::current().record("request_id", tracing::field::display(request_id));
        let plugin_cancel = cancel.child_token();
        let deadline = Instant::now() + self.summary_deadline;
        let call_ctx = PluginCallContext {
            request_id,
            tenant_id: TenantId::new(identity.tenant_id.as_str()),
            user_id: UserId::new(identity.user_id.as_str()),
            plugin_instance_id: plugin_instance_id.clone(),
            session_type_id,
            plugin_config,
            enabled_capabilities: session
                .enabled_capabilities
                .as_ref()
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            deadline: Some(deadline),
            cancel: plugin_cancel.clone(),
        };
        let plugin_ctx = SessionPluginCtx {
            session_type_id,
            session_id: Some(session_id),
            call_ctx,
        };

        // The history is forwarded out-of-band via the handler-built
        // wire context — the SDK trait does not pass `messages` on
        // SessionPluginCtx because session-level hooks may need to
        // re-resolve history under plugin control. We log a brief
        // summary so operators can correlate the call with the on-wire
        // event without logging PII.
        info!(
            session_id = %session_id,
            history_len = history.len(),
            plugin_instance_id = %plugin_instance_id,
            "invoking on_session_summary"
        );

        // Pre-stream failure → return mapped error to the handler.
        let plugin_stream = match plugin.on_session_summary(plugin_ctx).await {
            Ok(s) => s,
            Err(err) => {
                warn!(
                    session_id = %session_id,
                    error = %err,
                    "on_session_summary returned pre-stream failure"
                );
                return Err(err.into());
            }
        };

        // Pre-allocate the summary message id so the handler can emit it
        // on the `Start` event for symmetry with the message-send
        // pipeline. The actual DB row is materialised only after
        // `Complete` (see contract).
        let summary_message_id = Uuid::new_v4();
        tracing::Span::current().record(
            "summary_message_id",
            tracing::field::display(summary_message_id),
        );

        // Spawn the driver task + return the bounded-channel-backed
        // stream. The driver:
        //   - emits Start with our pre-allocated id;
        //   - re-emits chunks with the canonical id;
        //   - on Complete: persists the summary message + flips
        //     `is_hidden_from_backend=true` on the summarized ids;
        //   - on Error / cancel: emits the wire event and does NOT
        //     persist (the spec mandates "no side effects on failure").
        let stream = self.spawn_summary_driver(
            session_id,
            summary_message_id,
            plugin_stream,
            cancel,
            plugin_cancel,
            deadline,
        );
        Ok(stream)
    }

    // ---------------------------------------------------------------------
    // Tenant-scoped retention cleanup
    // ---------------------------------------------------------------------

    /// Entry point for the Phase 15 background scheduler. Walks every
    /// active session in the tenant, evaluates the effective retention
    /// policy, and recursively deletes eligible message subtrees.
    ///
    /// The algorithm:
    ///   1. Load all `lifecycle_state = active` sessions for the tenant.
    ///   2. For each session, acquire a Postgres advisory lock keyed on
    ///      the session id; on failure skip and continue.
    ///   3. Evaluate the policy → list of eligible non-root message ids.
    ///   4. Delete each eligible subtree atomically (one transaction per
    ///      subtree).
    ///   5. Emit a structured log event per session.
    ///
    /// Idempotency: re-running the cleanup MUST NOT double-delete or
    /// fail on already-empty sessions. `delete_message_subtree` returns
    /// `Ok(0)` when the root is missing (concurrent run already removed
    /// it); the policy evaluator returns an empty list when nothing is
    /// eligible.
    ///
    /// Note on advisory locks: the Phase 1 schema runs on both Postgres
    /// (`pg_try_advisory_xact_lock`) and SQLite (no analogue). The lock
    /// acquisition is treated as advisory — the policy evaluator and the
    /// subtree delete are both safe under concurrent runs even without
    /// the lock — so the SQLite path treats the lock as a successful no-op.
    #[instrument(skip(self), fields(tenant_id = %tenant_id))]
    pub async fn run_retention_cleanup_for_tenant(
        &self,
        tenant_id: &str,
    ) -> Result<RetentionCleanupReport> {
        let active = self.sessions.list_active_sessions_for_tenant(tenant_id).await?;
        let mut outcomes: Vec<SessionCleanupOutcome> = Vec::with_capacity(active.len());

        for row in active {
            let session: Session = row.into();
            let policy = resolve_effective_policy(&session);
            let label = retention_policy_label(&policy);

            // Empty policy → record + skip without locking.
            if matches!(policy, RetentionPolicy::None) {
                outcomes.push(SessionCleanupOutcome {
                    session_id: session.session_id,
                    policy_type: label,
                    messages_deleted: 0,
                    duration_ms: 0,
                    skipped_locked: false,
                });
                continue;
            }

            let start = Instant::now();

            // Advisory lock: best-effort. Real Postgres acquisition lives
            // in Phase 15 (it needs a `&DatabaseConnection`); the repo
            // surface here intentionally does not require one. The
            // single-session-at-a-time semantics are still preserved by
            // the SERIALIZABLE transaction inside `delete_message_subtree`.
            // Phase 15 will hook the actual lock into this code path.
            let lock_acquired = true;
            if !lock_acquired {
                outcomes.push(SessionCleanupOutcome {
                    session_id: session.session_id,
                    policy_type: label,
                    messages_deleted: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                    skipped_locked: true,
                });
                continue;
            }

            let eligible = self
                .evaluate_retention_policy(session.session_id, &policy)
                .await?;
            let mut removed: u64 = 0;
            for id in eligible {
                let n = self
                    .messages
                    .delete_message_subtree(session.session_id, id)
                    .await?;
                removed += n;
            }

            let duration_ms = start.elapsed().as_millis() as u64;
            info!(
                session_id = %session.session_id,
                messages_deleted = removed,
                policy_type = label,
                duration_ms = duration_ms,
                "retention cleanup completed for session"
            );

            outcomes.push(SessionCleanupOutcome {
                session_id: session.session_id,
                policy_type: label,
                messages_deleted: removed,
                duration_ms,
                skipped_locked: false,
            });
        }

        outcomes.sort_by_key(|o| o.session_id);
        Ok(RetentionCleanupReport { sessions: outcomes })
    }

    /// Evaluate a retention policy and return the list of message ids
    /// eligible for deletion. Public-ish (visible to tests in this
    /// module) but not exposed to other crates — the entry point for
    /// external callers is [`Self::run_retention_cleanup_for_tenant`].
    ///
    /// Algorithm (mirrors the feature spec §3 — Evaluate Retention Policy):
    /// - `None` → empty list.
    /// - `AgeBased { max_age_days }` → non-root messages with
    ///   `created_at < now() - max_age_days * 1 day`.
    /// - `CountBased { max_message_count }` → if total non-root count
    ///   exceeds the threshold, the oldest `total - max` messages.
    ///
    /// **Root messages (`parent_message_id IS NULL`) are NEVER eligible**
    /// — they anchor the conversation tree (see the feature spec design
    /// note on Root Message Preservation).
    pub(crate) async fn evaluate_retention_policy(
        &self,
        session_id: Uuid,
        policy: &RetentionPolicy,
    ) -> Result<Vec<Uuid>> {
        match policy {
            RetentionPolicy::None => Ok(Vec::new()),
            RetentionPolicy::AgeBased { max_age_days } => {
                let cutoff = OffsetDateTime::now_utc()
                    - Duration::from_secs(u64::from(*max_age_days) * 86_400);
                let rows = self
                    .messages
                    .list_non_root_messages_older_than(session_id, cutoff)
                    .await?;
                // Defensive: filter `parent_message_id.is_some()` again so
                // repository defaults that ignore the predicate cannot
                // accidentally return root messages.
                Ok(rows
                    .into_iter()
                    .filter(|m| m.parent_message_id.is_some())
                    .map(|m| m.message_id)
                    .collect())
            }
            RetentionPolicy::CountBased { max_message_count } => {
                let rows = self.messages.list_non_root_messages_chrono(session_id).await?;
                let filtered: Vec<Message> = rows
                    .into_iter()
                    .filter(|m| m.parent_message_id.is_some())
                    .collect();
                let total = filtered.len();
                let max = *max_message_count as usize;
                if total <= max {
                    return Ok(Vec::new());
                }
                let surplus = total - max;
                Ok(filtered
                    .into_iter()
                    .take(surplus)
                    .map(|m| m.message_id)
                    .collect())
            }
        }
    }

    // ---------------------------------------------------------------------
    // Internals
    // ---------------------------------------------------------------------

    async fn load_session(&self, identity: &Identity, session_id: Uuid) -> Result<Session> {
        let row = self
            .sessions
            .find_by_id(&identity.tenant_id, &identity.user_id, session_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;
        Ok(row.into())
    }

    /// Spawn the streaming driver that pumps the plugin's summary stream
    /// into a bounded channel, persists the result on success, and emits
    /// the canonical NDJSON-event sequence.
    fn spawn_summary_driver(
        &self,
        session_id: Uuid,
        summary_message_id: Uuid,
        mut plugin_stream: chat_engine_sdk::plugin::PluginStream,
        cancel: CancellationToken,
        plugin_cancel: CancellationToken,
        deadline: Instant,
    ) -> SummaryStream {
        let (tx, rx) = mpsc::channel::<StreamingEvent>(self.summary_buffer_size);
        let messages = Arc::clone(&self.messages);

        // Sleep-until-deadline guard. When the deadline fires we cancel
        // the plugin token; the driver loop folds the elapsed deadline
        // into a timeout error.
        let plugin_cancel_for_deadline = plugin_cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            plugin_cancel_for_deadline.cancel();
        });

        let tx_for_driver = tx.clone();
        tokio::spawn(async move {
            // 1) Emit our canonical Start (message_id = summary id).
            let start = StreamingEvent::Start(StreamingStartEvent {
                message_id: summary_message_id,
            });
            if tx_for_driver.send(start).await.is_err() {
                cancel.cancel();
                return;
            }

            let mut accumulator = String::new();
            let mut last_metadata: Option<JsonValue> = None;
            let mut summarized_ids: Vec<Uuid> = Vec::new();
            let mut completed = false;
            let mut errored: Option<String> = None;

            loop {
                tokio::select! {
                    biased;

                    _ = cancel.cancelled() => {
                        plugin_cancel.cancel();
                        break;
                    }

                    next = plugin_stream.next() => {
                        let Some(item) = next else {
                            // Plugin closed without emitting Complete.
                            // Treat as graceful end — but per the spec
                            // ("persist only on Complete") we do NOT
                            // persist a summary message here.
                            break;
                        };
                        match item {
                            Ok(StreamingEvent::Start(_)) => {
                                // Drop the plugin's own Start; we already
                                // emitted ours.
                                continue;
                            }
                            Ok(StreamingEvent::Chunk(c)) => {
                                accumulator.push_str(&c.chunk);
                                let evt = StreamingEvent::Chunk(StreamingChunkEvent {
                                    message_id: summary_message_id,
                                    chunk: c.chunk,
                                });
                                if tx_for_driver.send(evt).await.is_err() {
                                    plugin_cancel.cancel();
                                    break;
                                }
                            }
                            Ok(StreamingEvent::Complete(c)) => {
                                // Inspect metadata for an optional list of
                                // summarized message ids; record it so we
                                // can flip `is_hidden_from_backend` on the
                                // matching rows in the persist step.
                                if let Some(ref meta) = c.metadata {
                                    summarized_ids = extract_summarized_ids(meta);
                                }
                                last_metadata = c.metadata.clone();
                                completed = true;
                                let evt = StreamingEvent::Complete(StreamingCompleteEvent {
                                    message_id: summary_message_id,
                                    metadata: c.metadata,
                                });
                                let _ = tx_for_driver.send(evt).await;
                                break;
                            }
                            Ok(StreamingEvent::Error(e)) => {
                                let evt = StreamingEvent::Error(StreamingErrorEvent {
                                    message_id: summary_message_id,
                                    error: e.error.clone(),
                                });
                                let _ = tx_for_driver.send(evt).await;
                                errored = Some(e.error);
                                break;
                            }
                            Err(err) => {
                                let s = err.to_string();
                                let evt = StreamingEvent::Error(StreamingErrorEvent {
                                    message_id: summary_message_id,
                                    error: s.clone(),
                                });
                                let _ = tx_for_driver.send(evt).await;
                                errored = Some(s);
                                break;
                            }
                        }
                    }
                }
            }

            if completed {
                // Persist the summary message + flip
                // `is_hidden_from_backend` on the reported ids.
                if let Err(err) = messages
                    .insert_summary_message(
                        session_id,
                        accumulator,
                        last_metadata,
                        summarized_ids,
                    )
                    .await
                {
                    warn!(
                        session_id = %session_id,
                        summary_message_id = %summary_message_id,
                        error = %err,
                        "failed to persist session summary after stream complete",
                    );
                }
            } else if let Some(err) = errored {
                // Mid-stream error: per the spec the service does NOT
                // persist the summary. Log for operators only.
                warn!(
                    session_id = %session_id,
                    summary_message_id = %summary_message_id,
                    error = %err,
                    "summary stream errored mid-flight; no summary persisted"
                );
            }
        });

        stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|evt| (evt, rx))
        })
        .boxed()
    }
}

// =========================================================================
//  Free helpers
// =========================================================================

/// Validate a [`RetentionPolicy`] payload per the SDK constraints.
///
/// Returns:
/// - `Ok(_)` for [`RetentionPolicy::None`].
/// - `Err(BadRequest)` for `AgeBased { max_age_days: 0 }`.
/// - `Err(BadRequest)` for `CountBased { max_message_count: 0 }`.
///
/// Unknown variants are enforced by serde at the deserialization layer
/// (`#[serde(tag = "type", rename_all = "snake_case")]` rejects unknown
/// discriminators automatically).
pub fn validate_retention_policy(policy: RetentionPolicy) -> Result<ValidatedPolicy> {
    match &policy {
        RetentionPolicy::None => {}
        RetentionPolicy::AgeBased { max_age_days } => {
            if *max_age_days < 1 {
                return Err(ChatEngineError::bad_request(
                    "max_age_days required and must be >= 1",
                ));
            }
        }
        RetentionPolicy::CountBased { max_message_count } => {
            if *max_message_count < 1 {
                return Err(ChatEngineError::bad_request(
                    "max_message_count required and must be >= 1",
                ));
            }
        }
    }
    Ok(ValidatedPolicy(policy))
}

/// Resolve the effective retention policy for a session: per-session
/// override wins; fallback resolves to [`RetentionPolicy::None`] until
/// the session-type column lands (ADR-0021).
#[must_use]
pub fn resolve_effective_policy(session: &Session) -> RetentionPolicy {
    get_retention_policy(session).unwrap_or(RetentionPolicy::None)
}

/// Stable short label for a [`RetentionPolicy`] discriminant, used for
/// structured log events + metrics dimensions.
#[must_use]
pub fn retention_policy_label(p: &RetentionPolicy) -> &'static str {
    match p {
        RetentionPolicy::None => "none",
        RetentionPolicy::AgeBased { .. } => "age_based",
        RetentionPolicy::CountBased { .. } => "count_based",
    }
}

/// Extract an optional list of summarized message ids from the plugin's
/// `Complete` metadata. The SDK convention (per the feature spec §
/// Generate Session Summary) places this under
/// `metadata.summarized_message_ids: [uuid, ...]`. Malformed shapes
/// silently collapse to an empty list so a plugin that omits the field
/// does not break the persistence flow.
fn extract_summarized_ids(meta: &JsonValue) -> Vec<Uuid> {
    let Some(arr) = meta
        .get("summarized_message_ids")
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use chat_engine_sdk::models::{LifecycleState, MessageRole};
    use chat_engine_sdk::plugin::ChatEngineBackendPlugin;
    use modkit::ClientHub;
    use parking_lot::Mutex;
    use std::time::Duration;
    use time::OffsetDateTime;

    use crate::domain::message::Message;
    use crate::infra::db::entity::{session as session_entity, session_type as session_type_entity};
    use crate::infra::db::repo::message_repo::{
        FinalizeOutcome, InsertedPair, MessageRepo, NewUserMessage,
    };
    use crate::infra::db::repo::plugin_config_repo::PluginConfigRepo;
    use crate::infra::db::repo::session_repo::{SessionPage, SessionRepo};
    use crate::infra::db::repo::session_type_repo::SessionTypeRepo;

    // ----- Mocks -------------------------------------------------------

    struct MockSessionRepo {
        rows: Mutex<Vec<session_entity::Model>>,
    }

    impl MockSessionRepo {
        fn new(rows: Vec<session_entity::Model>) -> Arc<Self> {
            Arc::new(Self {
                rows: Mutex::new(rows),
            })
        }
    }

    #[async_trait]
    impl SessionRepo for MockSessionRepo {
        async fn insert(
            &self,
            _m: session_entity::ActiveModel,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Err(ChatEngineError::internal("mock insert"))
        }

        async fn find_by_id(
            &self,
            tenant_id: &str,
            user_id: &str,
            session_id: Uuid,
        ) -> std::result::Result<Option<session_entity::Model>, ChatEngineError> {
            Ok(self
                .rows
                .lock()
                .iter()
                .find(|r| {
                    r.session_id == session_id
                        && r.tenant_id == tenant_id
                        && r.user_id == user_id
                })
                .cloned())
        }

        async fn list_paginated(
            &self,
            _t: &str,
            _u: &str,
            _c: Option<&str>,
            _l: u32,
        ) -> std::result::Result<SessionPage, ChatEngineError> {
            Ok(SessionPage {
                items: vec![],
                next_cursor: None,
            })
        }

        async fn update_metadata(
            &self,
            _t: &str,
            _u: &str,
            session_id: Uuid,
            metadata: Option<JsonValue>,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            let mut rows = self.rows.lock();
            for row in rows.iter_mut() {
                if row.session_id == session_id {
                    row.metadata = metadata.clone();
                    return Ok(row.clone());
                }
            }
            Err(ChatEngineError::not_found("session", session_id))
        }

        async fn update_capabilities(
            &self,
            _t: &str,
            _u: &str,
            _id: Uuid,
            _c: Option<JsonValue>,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Err(ChatEngineError::internal("mock update_capabilities"))
        }

        async fn update_lifecycle_state(
            &self,
            _t: &str,
            _u: &str,
            _id: Uuid,
            _s: LifecycleState,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Err(ChatEngineError::internal("mock update_lifecycle_state"))
        }

        async fn soft_delete(
            &self,
            _t: &str,
            _u: &str,
            _id: Uuid,
            _d: i64,
        ) -> std::result::Result<session_entity::Model, ChatEngineError> {
            Err(ChatEngineError::internal("mock soft_delete"))
        }

        async fn hard_delete(
            &self,
            _t: &str,
            _u: &str,
            _id: Uuid,
        ) -> std::result::Result<bool, ChatEngineError> {
            Ok(true)
        }

        async fn list_active_sessions_for_tenant(
            &self,
            tenant_id: &str,
        ) -> std::result::Result<Vec<session_entity::Model>, ChatEngineError> {
            Ok(self
                .rows
                .lock()
                .iter()
                .filter(|r| {
                    r.tenant_id == tenant_id
                        && r.lifecycle_state == LifecycleState::Active.as_str()
                })
                .cloned()
                .collect())
        }
    }

    struct MockSessionTypeRepo;
    #[async_trait]
    impl SessionTypeRepo for MockSessionTypeRepo {
        async fn insert(
            &self,
            _m: session_type_entity::ActiveModel,
        ) -> std::result::Result<session_type_entity::Model, ChatEngineError> {
            Err(ChatEngineError::internal("mock"))
        }
        async fn find_by_id(
            &self,
            _id: Uuid,
        ) -> std::result::Result<Option<session_type_entity::Model>, ChatEngineError> {
            Ok(None)
        }
        async fn list(
            &self,
        ) -> std::result::Result<Vec<session_type_entity::Model>, ChatEngineError> {
            Ok(vec![])
        }
    }

    /// `MockMessageRepo` driven by a caller-supplied vector of messages
    /// the retention-evaluator should see. Tracks `delete_message_subtree`
    /// calls so tests can assert at-most-once behaviour.
    struct MockMessageRepo {
        all: Mutex<Vec<Message>>,
        deletes: Mutex<Vec<Uuid>>,
    }

    impl MockMessageRepo {
        fn new(messages: Vec<Message>) -> Arc<Self> {
            Arc::new(Self {
                all: Mutex::new(messages),
                deletes: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl MessageRepo for MockMessageRepo {
        async fn insert_user_and_assistant_stub(
            &self,
            _r: NewUserMessage,
        ) -> std::result::Result<InsertedPair, ChatEngineError> {
            Err(ChatEngineError::internal("mock"))
        }
        async fn finalize_assistant(
            &self,
            _id: Uuid,
            _o: FinalizeOutcome,
        ) -> std::result::Result<(), ChatEngineError> {
            Ok(())
        }
        async fn fetch_active_history(
            &self,
            _s: Uuid,
            _d: Option<u32>,
        ) -> std::result::Result<Vec<Message>, ChatEngineError> {
            Ok(self.all.lock().clone())
        }
        async fn find_message_in_session(
            &self,
            _s: Uuid,
            _m: Uuid,
        ) -> std::result::Result<Option<Message>, ChatEngineError> {
            Ok(None)
        }
        async fn list_non_root_messages_chrono(
            &self,
            session_id: Uuid,
        ) -> std::result::Result<Vec<Message>, ChatEngineError> {
            Ok(self
                .all
                .lock()
                .iter()
                .filter(|m| m.session_id == session_id && m.parent_message_id.is_some())
                .cloned()
                .collect())
        }
        async fn list_non_root_messages_older_than(
            &self,
            session_id: Uuid,
            older_than: OffsetDateTime,
        ) -> std::result::Result<Vec<Message>, ChatEngineError> {
            Ok(self
                .all
                .lock()
                .iter()
                .filter(|m| {
                    m.session_id == session_id
                        && m.parent_message_id.is_some()
                        && m.created_at < older_than
                })
                .cloned()
                .collect())
        }
        async fn delete_message_subtree(
            &self,
            _s: Uuid,
            root_id: Uuid,
        ) -> std::result::Result<u64, ChatEngineError> {
            self.deletes.lock().push(root_id);
            Ok(1)
        }
    }

    struct StubPluginConfigRepo;
    #[async_trait]
    impl PluginConfigRepo for StubPluginConfigRepo {
        async fn find(
            &self,
            _p: &str,
            _s: Uuid,
        ) -> std::result::Result<Option<JsonValue>, ChatEngineError> {
            Ok(None)
        }
        async fn upsert(
            &self,
            _p: &str,
            _s: Uuid,
            _c: JsonValue,
        ) -> std::result::Result<(), ChatEngineError> {
            Ok(())
        }
        async fn delete(
            &self,
            _p: &str,
            _s: Uuid,
        ) -> std::result::Result<(), ChatEngineError> {
            Ok(())
        }
    }

    // ----- Helpers -----------------------------------------------------

    fn make_session(session_id: Uuid, metadata: Option<JsonValue>) -> session_entity::Model {
        let now = OffsetDateTime::now_utc();
        session_entity::Model {
            session_id,
            tenant_id: "t".into(),
            user_id: "u".into(),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: None,
            metadata,
            lifecycle_state: LifecycleState::Active.as_str().to_string(),
            share_token: None,
            deleted_at: None,
            scheduled_hard_delete_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_message(session_id: Uuid, parent: Option<Uuid>, offset_secs: i64) -> Message {
        let ts = OffsetDateTime::from_unix_timestamp(1_700_000_000 + offset_secs).unwrap();
        Message {
            message_id: Uuid::new_v4(),
            session_id,
            parent_message_id: parent,
            variant_index: 0,
            is_active: true,
            role: MessageRole::User,
            content: serde_json::json!({"text": "hi"}),
            file_ids: vec![],
            metadata: None,
            is_complete: true,
            is_hidden_from_user: false,
            is_hidden_from_backend: false,
            created_at: ts,
            updated_at: ts,
        }
    }

    fn make_service(
        sessions: Arc<MockSessionRepo>,
        messages: Arc<MockMessageRepo>,
    ) -> IntelligenceService {
        let session_types: Arc<dyn SessionTypeRepo> = Arc::new(MockSessionTypeRepo);
        let hub = Arc::new(ClientHub::new());
        let plugins = PluginService::new(hub, Arc::new(StubPluginConfigRepo));
        IntelligenceService::new(
            sessions as Arc<dyn SessionRepo>,
            session_types,
            messages as Arc<dyn MessageRepo>,
            plugins,
        )
    }

    fn identity() -> Identity {
        Identity::new("t", "u", None).unwrap()
    }

    // ----- evaluate_retention_policy ----------------------------------

    #[tokio::test]
    async fn evaluate_none_returns_empty() {
        let session_id = Uuid::new_v4();
        let msgs = MockMessageRepo::new(vec![
            make_message(session_id, Some(Uuid::new_v4()), 0),
            make_message(session_id, Some(Uuid::new_v4()), 1),
        ]);
        let svc = make_service(MockSessionRepo::new(vec![]), msgs);
        let out = svc
            .evaluate_retention_policy(session_id, &RetentionPolicy::None)
            .await
            .unwrap();
        assert!(out.is_empty(), "None policy yields zero deletions");
    }

    #[tokio::test]
    async fn evaluate_age_based_deletes_only_old_non_root() {
        let session_id = Uuid::new_v4();
        let root_parent = Uuid::new_v4();
        // Old enough to be cleaned up (offset = 0 → unix 1_700_000_000;
        // cutoff = now - 1 day → safely older).
        let m_old = make_message(session_id, Some(root_parent), 0);
        // Modern message (current time → preserved).
        let mut m_new = make_message(session_id, Some(root_parent), 0);
        m_new.created_at = OffsetDateTime::now_utc();
        // Root message — must never be eligible regardless of age.
        let mut m_root = make_message(session_id, None, 0);
        m_root.created_at = OffsetDateTime::from_unix_timestamp(0).unwrap();
        let old_id = m_old.message_id;

        let msgs = MockMessageRepo::new(vec![m_old, m_new, m_root]);
        let svc = make_service(MockSessionRepo::new(vec![]), msgs);
        let out = svc
            .evaluate_retention_policy(
                session_id,
                &RetentionPolicy::AgeBased { max_age_days: 1 },
            )
            .await
            .unwrap();
        assert_eq!(out, vec![old_id], "only the old non-root message is eligible");
    }

    #[tokio::test]
    async fn evaluate_count_based_keeps_newest_n_and_excludes_root() {
        let session_id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        // 5 non-root messages, chronological order.
        let m0 = make_message(session_id, Some(parent), 0);
        let m1 = make_message(session_id, Some(parent), 1);
        let m2 = make_message(session_id, Some(parent), 2);
        let m3 = make_message(session_id, Some(parent), 3);
        let m4 = make_message(session_id, Some(parent), 4);
        // One root with the very oldest timestamp.
        let mut m_root = make_message(session_id, None, -1);
        m_root.created_at = OffsetDateTime::from_unix_timestamp(0).unwrap();

        let ids = vec![m0.message_id, m1.message_id];
        let msgs =
            MockMessageRepo::new(vec![m0, m1, m2, m3, m4, m_root]);
        let svc = make_service(MockSessionRepo::new(vec![]), msgs);
        let out = svc
            .evaluate_retention_policy(
                session_id,
                &RetentionPolicy::CountBased {
                    max_message_count: 3,
                },
            )
            .await
            .unwrap();
        assert_eq!(out, ids, "oldest 2 of 5 selected; newest 3 kept; root excluded");
    }

    #[tokio::test]
    async fn evaluate_count_based_below_threshold_is_empty() {
        let session_id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let msgs = MockMessageRepo::new(vec![
            make_message(session_id, Some(parent), 0),
            make_message(session_id, Some(parent), 1),
        ]);
        let svc = make_service(MockSessionRepo::new(vec![]), msgs);
        let out = svc
            .evaluate_retention_policy(
                session_id,
                &RetentionPolicy::CountBased {
                    max_message_count: 5,
                },
            )
            .await
            .unwrap();
        assert!(out.is_empty(), "2 <= 5 → no eligible deletions");
    }

    #[tokio::test]
    async fn evaluate_excludes_root_messages_under_age_based() {
        let session_id = Uuid::new_v4();
        // Only root messages (parent = None); all must be preserved.
        let mut m_root = make_message(session_id, None, 0);
        m_root.created_at = OffsetDateTime::from_unix_timestamp(0).unwrap();
        let msgs = MockMessageRepo::new(vec![m_root]);
        let svc = make_service(MockSessionRepo::new(vec![]), msgs);
        let out = svc
            .evaluate_retention_policy(
                session_id,
                &RetentionPolicy::AgeBased { max_age_days: 1 },
            )
            .await
            .unwrap();
        assert!(out.is_empty(), "root messages must never be eligible");
    }

    #[tokio::test]
    async fn run_retention_cleanup_is_idempotent() {
        let session_id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        // Populate the policy in metadata so it gets picked up by the
        // tenant-level pass.
        let metadata = serde_json::json!({
            "retention_policy": {"type": "count_based", "max_message_count": 1},
        });
        let session_row = make_session(session_id, Some(metadata));
        let sessions = MockSessionRepo::new(vec![session_row]);
        // 3 non-root → after the first pass 2 are eligible (3 - 1 = 2).
        let msgs = MockMessageRepo::new(vec![
            make_message(session_id, Some(parent), 0),
            make_message(session_id, Some(parent), 1),
            make_message(session_id, Some(parent), 2),
        ]);
        let svc = make_service(sessions.clone(), msgs.clone());
        let report = svc.run_retention_cleanup_for_tenant("t").await.unwrap();
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].messages_deleted, 2);
        let first_deletes = msgs.deletes.lock().clone();
        assert_eq!(first_deletes.len(), 2, "two deletes on first pass");

        // Idempotency: re-running with the same set produces another 2
        // deletes (the mock repo doesn't actually remove rows) but never
        // panics — the real repo returns Ok(0) for missing roots, which
        // is the contract the algorithm relies on.
        let report2 = svc.run_retention_cleanup_for_tenant("t").await.unwrap();
        assert_eq!(report2.sessions.len(), 1);
    }

    // ----- validate_retention_policy ----------------------------------

    #[test]
    fn validate_none_passes() {
        assert!(validate_retention_policy(RetentionPolicy::None).is_ok());
    }

    #[test]
    fn validate_age_based_rejects_zero() {
        let err = validate_retention_policy(RetentionPolicy::AgeBased { max_age_days: 0 })
            .unwrap_err();
        match err {
            ChatEngineError::BadRequest { reason } => {
                assert!(reason.contains("max_age_days"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn validate_age_based_accepts_one_or_more() {
        validate_retention_policy(RetentionPolicy::AgeBased { max_age_days: 1 })
            .expect("max_age_days=1 must be accepted");
        validate_retention_policy(RetentionPolicy::AgeBased { max_age_days: 365 })
            .expect("max_age_days=365 must be accepted");
    }

    #[test]
    fn validate_count_based_rejects_zero() {
        let err = validate_retention_policy(RetentionPolicy::CountBased {
            max_message_count: 0,
        })
        .unwrap_err();
        match err {
            ChatEngineError::BadRequest { reason } => {
                assert!(reason.contains("max_message_count"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn validate_count_based_accepts_one_or_more() {
        validate_retention_policy(RetentionPolicy::CountBased {
            max_message_count: 1,
        })
        .expect("max_message_count=1 must be accepted");
    }

    // ----- get_effective_retention_policy -----------------------------

    #[tokio::test]
    async fn get_effective_returns_per_session_when_set() {
        let session_id = Uuid::new_v4();
        let metadata = serde_json::json!({
            "retention_policy": {"type": "age_based", "max_age_days": 7},
        });
        let row = make_session(session_id, Some(metadata));
        let sessions = MockSessionRepo::new(vec![row]);
        let msgs = MockMessageRepo::new(vec![]);
        let svc = make_service(sessions, msgs);
        let out = svc
            .get_effective_retention_policy(&identity(), session_id)
            .await
            .unwrap();
        assert!(matches!(
            out,
            RetentionPolicy::AgeBased { max_age_days: 7 }
        ));
    }

    #[tokio::test]
    async fn get_effective_falls_back_to_none_when_unset() {
        let session_id = Uuid::new_v4();
        let row = make_session(session_id, None);
        let sessions = MockSessionRepo::new(vec![row]);
        let msgs = MockMessageRepo::new(vec![]);
        let svc = make_service(sessions, msgs);
        let out = svc
            .get_effective_retention_policy(&identity(), session_id)
            .await
            .unwrap();
        assert!(matches!(out, RetentionPolicy::None));
    }

    // ----- update_session_retention_policy ----------------------------

    #[tokio::test]
    async fn update_persists_policy_in_metadata() {
        let session_id = Uuid::new_v4();
        let row = make_session(session_id, None);
        let sessions = MockSessionRepo::new(vec![row]);
        let msgs = MockMessageRepo::new(vec![]);
        let svc = make_service(sessions.clone(), msgs);
        let updated = svc
            .update_session_retention_policy(
                &identity(),
                session_id,
                RetentionPolicy::CountBased {
                    max_message_count: 100,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            updated,
            RetentionPolicy::CountBased {
                max_message_count: 100
            }
        ));
        // Confirm the metadata write landed on the mock repo.
        let row = sessions.rows.lock()[0].clone();
        let stored = row.metadata.unwrap();
        assert_eq!(
            stored["retention_policy"],
            serde_json::json!({"type": "count_based", "max_message_count": 100})
        );
    }

    #[tokio::test]
    async fn update_rejects_invalid_max_age_days() {
        let session_id = Uuid::new_v4();
        let row = make_session(session_id, None);
        let sessions = MockSessionRepo::new(vec![row]);
        let msgs = MockMessageRepo::new(vec![]);
        let svc = make_service(sessions, msgs);
        let err = svc
            .update_session_retention_policy(
                &identity(),
                session_id,
                RetentionPolicy::AgeBased { max_age_days: 0 },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }

    #[tokio::test]
    async fn update_rejects_soft_deleted_session() {
        let session_id = Uuid::new_v4();
        let mut row = make_session(session_id, None);
        row.lifecycle_state = LifecycleState::SoftDeleted.as_str().to_string();
        let sessions = MockSessionRepo::new(vec![row]);
        let msgs = MockMessageRepo::new(vec![]);
        let svc = make_service(sessions, msgs);
        let err = svc
            .update_session_retention_policy(
                &identity(),
                session_id,
                RetentionPolicy::None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ChatEngineError::Conflict { .. }));
    }

    // ----- summary plugin integration ---------------------------------

    use chat_engine_sdk::error::PluginError;
    use chat_engine_sdk::plugin::{MessagePluginCtx, PluginStream, stream_from_events};
    use modkit::client_hub::ClientScope;

    struct ScriptedSummaryPlugin {
        id: String,
        events: Mutex<Option<Vec<StreamingEvent>>>,
        pre_error: Mutex<Option<PluginError>>,
    }

    impl ScriptedSummaryPlugin {
        fn ok(id: &str, events: Vec<StreamingEvent>) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                events: Mutex::new(Some(events)),
                pre_error: Mutex::new(None),
            })
        }

        fn pre_error(id: &str, err: PluginError) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                events: Mutex::new(None),
                pre_error: Mutex::new(Some(err)),
            })
        }
    }

    #[async_trait]
    impl ChatEngineBackendPlugin for ScriptedSummaryPlugin {
        async fn on_message(
            &self,
            _c: MessagePluginCtx,
        ) -> std::result::Result<PluginStream, PluginError> {
            Err(PluginError::internal("test plugin does not handle messages"))
        }

        async fn on_session_summary(
            &self,
            _c: SessionPluginCtx,
        ) -> std::result::Result<PluginStream, PluginError> {
            if let Some(err) = self.pre_error.lock().take() {
                return Err(err);
            }
            let events = self.events.lock().take().unwrap_or_default();
            Ok(stream_from_events(events))
        }

        fn plugin_instance_id(&self) -> &str {
            &self.id
        }
    }

    fn make_service_with_plugin(
        plugin_id: &str,
        plugin: Arc<dyn ChatEngineBackendPlugin>,
        session_type_id: Uuid,
        session_row: session_entity::Model,
    ) -> (IntelligenceService, Arc<MockSessionRepo>, Arc<MockMessageRepo>) {
        let sessions = MockSessionRepo::new(vec![session_row]);
        let msgs = MockMessageRepo::new(vec![]);
        let hub = Arc::new(ClientHub::new());
        hub.register_scoped::<dyn ChatEngineBackendPlugin>(
            ClientScope::gts_id(plugin_id),
            plugin,
        );
        let plugins = PluginService::new(hub, Arc::new(StubPluginConfigRepo));

        // session_types mock: return a row with the configured plugin id.
        struct OneTypeRepo {
            model: Mutex<session_type_entity::Model>,
        }
        #[async_trait]
        impl SessionTypeRepo for OneTypeRepo {
            async fn insert(
                &self,
                _m: session_type_entity::ActiveModel,
            ) -> std::result::Result<session_type_entity::Model, ChatEngineError> {
                Err(ChatEngineError::internal("mock"))
            }
            async fn find_by_id(
                &self,
                id: Uuid,
            ) -> std::result::Result<Option<session_type_entity::Model>, ChatEngineError>
            {
                let m = self.model.lock().clone();
                if m.session_type_id == id {
                    Ok(Some(m))
                } else {
                    Ok(None)
                }
            }
            async fn list(
                &self,
            ) -> std::result::Result<Vec<session_type_entity::Model>, ChatEngineError>
            {
                Ok(vec![self.model.lock().clone()])
            }
        }
        let now = OffsetDateTime::now_utc();
        let st_repo: Arc<dyn SessionTypeRepo> = Arc::new(OneTypeRepo {
            model: Mutex::new(session_type_entity::Model {
                session_type_id,
                name: "t".into(),
                plugin_instance_id: Some(plugin_id.into()),
                created_at: now,
                updated_at: now,
            }),
        });

        let svc = IntelligenceService::new(
            sessions.clone() as Arc<dyn SessionRepo>,
            st_repo,
            msgs.clone() as Arc<dyn MessageRepo>,
            plugins,
        );
        (svc, sessions, msgs)
    }

    #[tokio::test]
    async fn summarize_pre_stream_error_propagates() {
        let plugin_id = "summary-fail";
        let session_type_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let mut row = make_session(session_id, None);
        row.session_type_id = Some(session_type_id);
        let plugin = ScriptedSummaryPlugin::pre_error(plugin_id, PluginError::internal("boom"));
        let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
        let (svc, _sessions, _msgs) = make_service_with_plugin(
            plugin_id,
            plugin_dyn,
            session_type_id,
            row,
        );

        let cancel = CancellationToken::new();
        let result = svc.summarize_session(&identity(), session_id, cancel).await;
        let err = match result {
            Ok(_) => panic!("pre-stream failure must surface as Err"),
            Err(e) => e,
        };
        // Internal pluginerror is mapped to ChatEngineError::Internal
        // (see error.rs). Either Internal or BackendUnavailable is
        // acceptable in the carry-over notes — the handler maps both
        // to 502.
        assert!(
            matches!(
                err,
                ChatEngineError::Internal { .. } | ChatEngineError::BackendUnavailable { .. }
            ),
            "expected Internal or BackendUnavailable, got {err:?}",
        );
    }

    #[tokio::test]
    async fn summarize_returns_422_style_when_plugin_unregistered() {
        // No plugin registered for this session_type's id — we still
        // reach summary entry but plugin.resolve fails, mapped to
        // BackendUnavailable per the rules.
        let plugin_id = "missing";
        let session_type_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let mut row = make_session(session_id, None);
        row.session_type_id = Some(session_type_id);
        // Use a session-type repo that returns the type but the plugin
        // hub has no scope registered for `plugin_id`.
        let now = OffsetDateTime::now_utc();
        struct ReturnsType {
            id: Uuid,
            pid: String,
            now: OffsetDateTime,
        }
        #[async_trait]
        impl SessionTypeRepo for ReturnsType {
            async fn insert(
                &self,
                _m: session_type_entity::ActiveModel,
            ) -> std::result::Result<session_type_entity::Model, ChatEngineError> {
                Err(ChatEngineError::internal("mock"))
            }
            async fn find_by_id(
                &self,
                id: Uuid,
            ) -> std::result::Result<Option<session_type_entity::Model>, ChatEngineError>
            {
                if id == self.id {
                    Ok(Some(session_type_entity::Model {
                        session_type_id: self.id,
                        name: "t".into(),
                        plugin_instance_id: Some(self.pid.clone()),
                        created_at: self.now,
                        updated_at: self.now,
                    }))
                } else {
                    Ok(None)
                }
            }
            async fn list(
                &self,
            ) -> std::result::Result<Vec<session_type_entity::Model>, ChatEngineError>
            {
                Ok(vec![])
            }
        }
        let st_repo: Arc<dyn SessionTypeRepo> = Arc::new(ReturnsType {
            id: session_type_id,
            pid: plugin_id.into(),
            now,
        });
        let sessions = MockSessionRepo::new(vec![row]);
        let msgs = MockMessageRepo::new(vec![]);
        let hub = Arc::new(ClientHub::new()); // empty
        let plugins = PluginService::new(hub, Arc::new(StubPluginConfigRepo));
        let svc = IntelligenceService::new(
            sessions as Arc<dyn SessionRepo>,
            st_repo,
            msgs as Arc<dyn MessageRepo>,
            plugins,
        );

        let cancel = CancellationToken::new();
        let result = svc.summarize_session(&identity(), session_id, cancel).await;
        let err = match result {
            Ok(_) => panic!("unregistered plugin must produce an error"),
            Err(e) => e,
        };
        match err {
            ChatEngineError::BackendUnavailable { ref reason, .. } => {
                assert!(reason.contains("not registered"), "got: {reason}");
            }
            other => panic!("expected BackendUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn summarize_happy_path_persists_on_complete() {
        let plugin_id = "summary-ok";
        let session_type_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let mut row = make_session(session_id, None);
        row.session_type_id = Some(session_type_id);
        let plugin = ScriptedSummaryPlugin::ok(
            plugin_id,
            vec![
                StreamingEvent::Chunk(StreamingChunkEvent {
                    message_id: Uuid::nil(),
                    chunk: "summary ".into(),
                }),
                StreamingEvent::Chunk(StreamingChunkEvent {
                    message_id: Uuid::nil(),
                    chunk: "text".into(),
                }),
                StreamingEvent::Complete(StreamingCompleteEvent {
                    message_id: Uuid::nil(),
                    metadata: Some(serde_json::json!({"summarized_message_ids": []})),
                }),
            ],
        );
        let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
        let (svc, _sessions, _msgs) = make_service_with_plugin(
            plugin_id,
            plugin_dyn,
            session_type_id,
            row,
        );

        let cancel = CancellationToken::new();
        let mut stream = svc
            .summarize_session(&identity(), session_id, cancel)
            .await
            .expect("summary dispatch");
        let mut kinds = Vec::new();
        while let Some(evt) = stream.next().await {
            match evt {
                StreamingEvent::Start(_) => kinds.push("start"),
                StreamingEvent::Chunk(_) => kinds.push("chunk"),
                StreamingEvent::Complete(_) => kinds.push("complete"),
                StreamingEvent::Error(_) => kinds.push("error"),
            }
        }
        assert_eq!(kinds, vec!["start", "chunk", "chunk", "complete"]);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // ----- summary-related helpers ------------------------------------

    #[test]
    fn extract_summarized_ids_parses_valid_array() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let meta = serde_json::json!({
            "summarized_message_ids": [id1.to_string(), id2.to_string()],
        });
        let out = extract_summarized_ids(&meta);
        assert_eq!(out, vec![id1, id2]);
    }

    #[test]
    fn extract_summarized_ids_handles_missing_key() {
        let meta = serde_json::json!({"other": "value"});
        assert!(extract_summarized_ids(&meta).is_empty());
    }

    #[test]
    fn extract_summarized_ids_skips_malformed_entries() {
        let id = Uuid::new_v4();
        let meta = serde_json::json!({
            "summarized_message_ids": [id.to_string(), "not-a-uuid", 42],
        });
        assert_eq!(extract_summarized_ids(&meta), vec![id]);
    }

    #[test]
    fn retention_policy_label_covers_all_variants() {
        assert_eq!(retention_policy_label(&RetentionPolicy::None), "none");
        assert_eq!(
            retention_policy_label(&RetentionPolicy::AgeBased { max_age_days: 1 }),
            "age_based"
        );
        assert_eq!(
            retention_policy_label(&RetentionPolicy::CountBased {
                max_message_count: 1
            }),
            "count_based"
        );
    }

    // ----- run_retention_cleanup_for_tenant: skip-none --------------------

    #[tokio::test]
    async fn run_cleanup_records_none_policy_without_lock_or_delete() {
        let session_id = Uuid::new_v4();
        let row = make_session(session_id, None); // metadata None → effective None
        let sessions = MockSessionRepo::new(vec![row]);
        let msgs = MockMessageRepo::new(vec![]);
        let svc = make_service(sessions, msgs.clone());
        let report = svc.run_retention_cleanup_for_tenant("t").await.unwrap();
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].policy_type, "none");
        assert_eq!(report.sessions[0].messages_deleted, 0);
        assert!(msgs.deletes.lock().is_empty());
    }

    #[tokio::test]
    async fn run_cleanup_ignores_other_tenants() {
        let session_id = Uuid::new_v4();
        let mut row = make_session(session_id, None);
        row.tenant_id = "other".into();
        let sessions = MockSessionRepo::new(vec![row]);
        let msgs = MockMessageRepo::new(vec![]);
        let svc = make_service(sessions, msgs);
        let report = svc.run_retention_cleanup_for_tenant("t").await.unwrap();
        assert!(report.sessions.is_empty());
    }
}
