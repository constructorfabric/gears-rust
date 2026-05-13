use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::PluginError;
use crate::models::{
    Capability, CapabilityValue, HealthStatus, Message, StreamingEvent, TenantId, UserId,
};

/// A boxed async stream of streaming events from a plugin.
///
/// Each item is a `Result`, so individual events can fail (e.g., mid-stream
/// network error) without aborting the stream. The outer `Result<PluginStream, _>`
/// returned by the trait methods represents errors that occur *before* the stream
/// starts (e.g., invalid config, plugin unavailable).
///
/// # `'static` lifetime requirement
///
/// `PluginStream` is `BoxStream<'static, _>`, meaning the stream must own
/// everything it touches. Chat Engine drives the stream to completion *after*
/// the trait method returns, so any reference into `&self` would dangle once
/// the call frame unwinds.
///
/// The compiler error you will see if you violate this is a lifetime mismatch
/// pointing at the inside of your `async_stream::stream! { … }`,
/// `futures::stream::unfold(…)`, or async closure — *not* at the trait
/// signature. The fix is to detach from `&self` before entering the stream body:
///
/// ```ignore
/// // ❌ Captures `&self.config` — won't satisfy `'static`.
/// async fn on_message(&self, ctx: MessagePluginCtx)
///     -> Result<PluginStream, PluginError>
/// {
///     Ok(async_stream::stream! {
///         let response = self.config.client.send(&ctx.messages).await?;
///         // ...
///     }.boxed())
/// }
///
/// // ✅ Clone the bits you need out of `self` first.
/// async fn on_message(&self, ctx: MessagePluginCtx)
///     -> Result<PluginStream, PluginError>
/// {
///     let client = self.config.client.clone();
///     Ok(async_stream::stream! {
///         let response = client.send(&ctx.messages).await?;
///         // ...
///     }.boxed())
/// }
///
/// // ✅ Or hold the plugin in an `Arc` and clone the handle.
/// // (works well if you need many fields and `Clone` on each is awkward)
/// // self: Arc<MyPlugin> at the call site, then:
/// let me = Arc::clone(&self);
/// Ok(async_stream::stream! {
///     me.do_things(...).await;
/// }.boxed())
/// ```
///
/// For non-streaming responses, prefer [`stream_from_events`] — it side-steps
/// the issue entirely by collecting all events synchronously before returning.
pub type PluginStream = BoxStream<'static, Result<StreamingEvent, PluginError>>;

/// Helper to build an empty plugin stream (default no-op responses).
#[must_use]
pub fn empty_stream() -> PluginStream {
    stream::empty().boxed()
}

/// Helper to build a plugin stream from a pre-collected vector of events.
///
/// Useful for non-streaming plugins or stub implementations that produce all
/// events up-front.
#[must_use]
pub fn stream_from_events(events: Vec<StreamingEvent>) -> PluginStream {
    stream::iter(events.into_iter().map(Ok)).boxed()
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct SessionPluginCtx {
    pub session_type_id: Uuid,
    /// `None` during [`ChatEngineBackendPlugin::on_session_type_configured`]
    /// (no session exists yet); `Some` for all other lifecycle hooks
    /// (`on_session_created`, `on_session_updated`, `on_session_summary`).
    pub session_id: Option<Uuid>,
    pub call_ctx: PluginCallContext,
}

/// Context passed to message-handling plugin methods.
///
/// `Debug` is implemented manually to redact `messages` — message content is
/// PII (user prompts, assistant responses) that must never appear in logs.
/// The summary surfaces `len` and a per-role count so observability is
/// preserved without leaking text. `call_ctx` keeps its own redaction (see
/// `PluginCallContext::Debug`).
#[allow(clippy::module_name_repetitions)]
#[derive(Clone)]
pub struct MessagePluginCtx {
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub messages: Vec<Message>,
    pub call_ctx: PluginCallContext,
}

impl std::fmt::Debug for MessagePluginCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Per-role summary: counts how many user / assistant / system messages
        // are present so plugins/operators can sanity-check the call shape
        // without seeing any message content.
        let mut user = 0_usize;
        let mut assistant = 0_usize;
        let mut system = 0_usize;
        for m in &self.messages {
            match m.role {
                crate::models::MessageRole::User => user += 1,
                crate::models::MessageRole::Assistant => assistant += 1,
                crate::models::MessageRole::System => system += 1,
            }
        }
        let messages_summary = format_args!(
            "<redacted: {} message(s); user={user}, assistant={assistant}, system={system}>",
            self.messages.len()
        )
        .to_string();
        f.debug_struct("MessagePluginCtx")
            .field("session_id", &self.session_id)
            .field("message_id", &self.message_id)
            .field("messages", &messages_summary)
            .field("call_ctx", &self.call_ctx)
            .finish()
    }
}

/// Shared context attached to every plugin invocation.
///
/// `Debug` is implemented manually to redact `plugin_config` — it may contain
/// secrets (API keys, webhook auth, credentials) that must never hit logs.
/// Wrappers `SessionPluginCtx` / `MessagePluginCtx` derive `Debug` and
/// transitively inherit this redaction.
#[allow(clippy::module_name_repetitions)]
#[derive(Clone)]
pub struct PluginCallContext {
    /// Correlation ID for this plugin invocation. Used for log correlation and
    /// distributed tracing; Chat Engine generates a fresh UUIDv4 per call (or
    /// may propagate an upstream correlation ID). Plugins should include this
    /// in every log line emitted while handling the call.
    pub request_id: Uuid,
    /// Tenant that owns the session issuing the call.
    pub tenant_id: TenantId,
    /// End-user behind the call (opaque string from the auth token).
    pub user_id: UserId,
    /// GTS plugin instance ID that is handling the call (matches the bound
    /// `SessionType.plugin_instance_id`).
    pub plugin_instance_id: String,
    /// Session type the call is scoped to.
    pub session_type_id: Uuid,
    /// Opaque plugin-specific configuration loaded from `plugin_configs` for
    /// this `(plugin_instance_id, session_type_id)` pair.
    pub plugin_config: Option<serde_json::Value>,
    /// Capability values selected for this call (subset of those declared by
    /// the plugin via `Capability`).
    pub enabled_capabilities: Option<Vec<CapabilityValue>>,
    /// Absolute monotonic deadline for this plugin call. Plugins should bound
    /// long-running work (HTTP requests, retries) to remain within this budget.
    /// `None` means Chat Engine did not set a deadline.
    ///
    /// Use `remaining()` for a convenient countdown duration.
    pub deadline: Option<Instant>,
    /// Cooperative cancellation signal. Cancelled by Chat Engine when:
    /// - the client disconnects (HTTP stream closed)
    /// - the deadline elapses (Chat Engine bridges deadline → cancel)
    /// - explicit `DELETE /streaming` is invoked on a session
    ///
    /// Plugins should `select!` on `cancel.cancelled()` alongside their work
    /// and return `PluginError::Transient("cancelled")` (or similar) when
    /// the signal fires. `cancel.is_cancelled()` is also available for
    /// pre-flight checks before expensive operations.
    ///
    /// Clones of this token share the same cancellation state. When Chat Engine
    /// cancels, all clones observe the signal simultaneously — and conversely,
    /// calling `.cancel()` on any clone (including one obtained by cloning the
    /// enclosing `PluginCallContext`) cancels every other holder, including
    /// Chat Engine's parent token. If you fan out concurrent sub-tasks that
    /// need *independent* cancellation, derive child tokens with
    /// [`CancellationToken::child_token`] rather than cloning.
    pub cancel: CancellationToken,
}

impl PluginCallContext {
    /// True if cancellation has been signalled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Remaining time until the deadline.
    ///
    /// - `None` — no deadline was set; the plugin may use its own default budget.
    /// - `Some(Duration::ZERO)` — deadline has already elapsed; the plugin
    ///   should abort immediately (typically returning `PluginError::timeout()`).
    /// - `Some(d)` where `d > 0` — `d` of budget remains. Plugins typically
    ///   pass this to `tokio::time::timeout(...)` or `reqwest::Client::timeout(...)`.
    ///
    /// **Important**: collapsing "no deadline" and "elapsed" into `None`
    /// would be a footgun (`.unwrap_or(default)` would let elapsed deadlines
    /// silently extend their budget). Callers must handle the three cases
    /// distinctly.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline.map(|d| {
            d.checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO)
        })
    }
}

impl std::fmt::Debug for PluginCallContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `plugin_config` is redacted because it can carry plugin secrets
        // (API keys, webhook auth, credentials) that must never appear in logs.
        // We still indicate presence/absence so observability is not lost.
        let plugin_config_redacted: Option<&'static str> =
            self.plugin_config.as_ref().map(|_| "<redacted>");
        f.debug_struct("PluginCallContext")
            .field("request_id", &self.request_id)
            .field("tenant_id", &self.tenant_id)
            .field("user_id", &self.user_id)
            .field("plugin_instance_id", &self.plugin_instance_id)
            .field("session_type_id", &self.session_type_id)
            .field("plugin_config", &plugin_config_redacted)
            .field("enabled_capabilities", &self.enabled_capabilities)
            .field("remaining", &self.remaining())
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod plugin_call_context_tests {
    use super::{CancellationToken, Duration, Instant, PluginCallContext, TenantId, UserId};
    use uuid::Uuid;

    fn make_ctx() -> PluginCallContext {
        PluginCallContext {
            request_id: Uuid::nil(),
            tenant_id: TenantId::new("t"),
            user_id: UserId::new("u"),
            plugin_instance_id: "p".into(),
            session_type_id: Uuid::nil(),
            plugin_config: None,
            enabled_capabilities: None,
            deadline: None,
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn debug_redacts_plugin_config_when_present() {
        let mut ctx = make_ctx();
        ctx.plugin_config = Some(serde_json::json!({"api_key": "super-secret-123"}));
        let printed = format!("{ctx:?}");
        assert!(printed.contains("<redacted>"), "got: {printed}");
        assert!(
            !printed.contains("super-secret-123"),
            "secret leaked: {printed}"
        );
    }

    #[test]
    fn debug_prints_none_when_plugin_config_absent() {
        let ctx = make_ctx();
        let printed = format!("{ctx:?}");
        assert!(printed.contains("plugin_config: None"), "got: {printed}");
        assert!(!printed.contains("<redacted>"), "got: {printed}");
    }

    #[test]
    fn is_cancelled_reflects_token_state() {
        let ctx = make_ctx();
        assert!(!ctx.is_cancelled());
        ctx.cancel.cancel();
        assert!(ctx.is_cancelled());
    }

    #[test]
    fn remaining_is_none_when_no_deadline() {
        let ctx = make_ctx();
        assert!(ctx.remaining().is_none());
    }

    #[test]
    fn remaining_returns_positive_duration_for_future_deadline() {
        let mut ctx = make_ctx();
        ctx.deadline = Some(Instant::now() + Duration::from_secs(10));
        let r = ctx.remaining().expect("should be set");
        assert!(r > Duration::from_secs(5) && r <= Duration::from_secs(10));
    }

    #[test]
    fn remaining_is_zero_when_deadline_already_elapsed() {
        let mut ctx = make_ctx();
        ctx.deadline = Some(Instant::now() - Duration::from_secs(1));
        // Elapsed deadlines must be Some(ZERO), not None — the latter would
        // be indistinguishable from "no deadline set" and let plugins
        // silently extend their budget via `.unwrap_or(default)`.
        assert_eq!(ctx.remaining(), Some(Duration::ZERO));
    }

    #[test]
    fn remaining_is_zero_at_exact_deadline() {
        let mut ctx = make_ctx();
        ctx.deadline = Some(Instant::now());
        // Race-tolerant: anything <= 0 is Some(ZERO); a tiny positive value
        // is also acceptable but should be sub-millisecond.
        let r = ctx.remaining().expect("deadline is set");
        assert!(
            r <= Duration::from_millis(1),
            "expected ~ZERO, got {r:?}"
        );
    }
}

#[cfg(test)]
mod message_plugin_ctx_debug_tests {
    use super::{CancellationToken, MessagePluginCtx, PluginCallContext, TenantId, UserId};
    use crate::models::{Message, MessageRole};
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn make_message(role: MessageRole, secret_text: &str) -> Message {
        let now = OffsetDateTime::now_utc();
        Message {
            message_id: Uuid::nil(),
            session_id: Uuid::nil(),
            parent_message_id: None,
            variant_index: 0,
            is_active: true,
            role,
            content: serde_json::json!({ "text": secret_text }),
            file_ids: vec![],
            metadata: None,
            is_complete: true,
            is_hidden_from_user: false,
            is_hidden_from_backend: false,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_call_ctx() -> PluginCallContext {
        PluginCallContext {
            request_id: Uuid::nil(),
            tenant_id: TenantId::new("t"),
            user_id: UserId::new("u"),
            plugin_instance_id: "p".into(),
            session_type_id: Uuid::nil(),
            plugin_config: None,
            enabled_capabilities: None,
            deadline: None,
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn debug_redacts_message_content_but_shows_per_role_summary() {
        let ctx = MessagePluginCtx {
            session_id: Uuid::nil(),
            message_id: Uuid::nil(),
            messages: vec![
                make_message(MessageRole::System, "system-secret-prompt"),
                make_message(MessageRole::User, "i had a heart attack last night"),
                make_message(MessageRole::Assistant, "private-response-PII"),
                make_message(MessageRole::User, "another sensitive question"),
            ],
            call_ctx: make_call_ctx(),
        };
        let printed = format!("{ctx:?}");

        // PII never appears in Debug output.
        assert!(
            !printed.contains("system-secret-prompt"),
            "system content leaked: {printed}"
        );
        assert!(
            !printed.contains("heart attack"),
            "user PII leaked: {printed}"
        );
        assert!(
            !printed.contains("private-response-PII"),
            "assistant content leaked: {printed}"
        );
        assert!(
            !printed.contains("sensitive question"),
            "user content leaked: {printed}"
        );

        // Summary still gives observability.
        assert!(printed.contains("4 message(s)"), "got: {printed}");
        assert!(printed.contains("user=2"), "got: {printed}");
        assert!(printed.contains("assistant=1"), "got: {printed}");
        assert!(printed.contains("system=1"), "got: {printed}");
        assert!(printed.contains("<redacted"), "got: {printed}");
    }

    #[test]
    fn debug_shows_zero_counts_for_empty_history() {
        let ctx = MessagePluginCtx {
            session_id: Uuid::nil(),
            message_id: Uuid::nil(),
            messages: vec![],
            call_ctx: make_call_ctx(),
        };
        let printed = format!("{ctx:?}");
        assert!(printed.contains("0 message(s)"), "got: {printed}");
        assert!(printed.contains("user=0"), "got: {printed}");
    }
}

#[async_trait]
pub trait ChatEngineBackendPlugin: Send + Sync {
    async fn on_session_type_configured(
        &self,
        _ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        Ok(vec![])
    }

    async fn on_session_created(
        &self,
        _ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        Ok(vec![])
    }

    async fn on_session_updated(
        &self,
        _ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        Ok(vec![])
    }

    /// Process a new user message and stream response events back.
    ///
    /// The outer `Result` reports failures *before* streaming starts (e.g., auth
    /// failure). Once a stream is returned, individual items may be `Err` to
    /// signal mid-stream failures (e.g., upstream disconnect).
    ///
    /// The returned [`PluginStream`] must be `'static` — it cannot borrow from
    /// `&self`. See [`PluginStream`]'s docs for the idiomatic way to detach
    /// captured state (clone fields out, or hold `self` in an `Arc`).
    async fn on_message(
        &self,
        _ctx: MessagePluginCtx,
    ) -> Result<PluginStream, PluginError> {
        Ok(empty_stream())
    }

    /// Regenerate a response for an existing user message (new variant).
    ///
    /// Same streaming semantics as `on_message`.
    async fn on_message_recreate(
        &self,
        _ctx: MessagePluginCtx,
    ) -> Result<PluginStream, PluginError> {
        Ok(empty_stream())
    }

    /// Generate a session summary and stream the result back.
    ///
    /// Summary plugins typically emit one or more `Chunk` events followed by a
    /// `Complete` event carrying metadata.
    async fn on_session_summary(
        &self,
        _ctx: SessionPluginCtx,
    ) -> Result<PluginStream, PluginError> {
        Ok(empty_stream())
    }

    async fn health_check(&self) -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    fn plugin_instance_id(&self) -> &str;
}
