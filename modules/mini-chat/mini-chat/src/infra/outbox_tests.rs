use super::*;
use mini_chat_sdk::{
    MiniChatAuditPluginClientV1, MiniChatAuditPluginError, MiniChatModelPolicyPluginClientV1,
    MiniChatModelPolicyPluginError, PolicySnapshot, PolicyVersionInfo, PublishError,
    TurnAuditEvent, TurnDeleteAuditEvent, TurnEditAuditEvent, TurnRetryAuditEvent, UserLimits,
};
use modkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use std::sync::atomic::{AtomicU32, Ordering};
use time::OffsetDateTime;
use uuid::Uuid;

fn make_usage_event() -> UsageEvent {
    UsageEvent {
        tenant_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        chat_id: Uuid::new_v4(),
        turn_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        effective_model: "gpt-4o".to_owned(),
        selected_model: "gpt-4o".to_owned(),
        terminal_state: "completed".to_owned(),
        billing_outcome: "charged".to_owned(),
        usage: None,
        actual_credits_micro: 500,
        settlement_method: "quota".to_owned(),
        policy_version_applied: 1,
        web_search_calls: 0,
        code_interpreter_calls: 0,
        timestamp: OffsetDateTime::now_utc(),
    }
}

fn make_outbox_message(payload: Vec<u8>) -> OutboxMessage {
    OutboxMessage {
        partition_id: 1,
        seq: 42,
        payload,
        payload_type: "application/json".to_owned(),
        created_at: chrono::Utc::now(),
        attempts: 0,
    }
}

/// Mock plugin that records `publish_usage` calls and returns a configurable result.
struct MockPlugin {
    result: std::sync::Mutex<Result<(), PublishError>>,
    call_count: AtomicU32,
    notifier: tokio::sync::Notify,
}

impl MockPlugin {
    fn ok() -> Arc<Self> {
        Arc::new(Self {
            result: std::sync::Mutex::new(Ok(())),
            call_count: AtomicU32::new(0),
            notifier: tokio::sync::Notify::new(),
        })
    }

    fn transient(reason: &str) -> Arc<Self> {
        Arc::new(Self {
            result: std::sync::Mutex::new(Err(PublishError::Transient(reason.to_owned()))),
            call_count: AtomicU32::new(0),
            notifier: tokio::sync::Notify::new(),
        })
    }

    fn permanent(reason: &str) -> Arc<Self> {
        Arc::new(Self {
            result: std::sync::Mutex::new(Err(PublishError::Permanent(reason.to_owned()))),
            call_count: AtomicU32::new(0),
            notifier: tokio::sync::Notify::new(),
        })
    }

    fn calls(&self) -> u32 {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl MiniChatModelPolicyPluginClientV1 for MockPlugin {
    async fn get_current_policy_version(
        &self,
        _user_id: Uuid,
    ) -> Result<PolicyVersionInfo, MiniChatModelPolicyPluginError> {
        unimplemented!("not needed in outbox tests")
    }

    async fn get_policy_snapshot(
        &self,
        _user_id: Uuid,
        _policy_version: u64,
    ) -> Result<PolicySnapshot, MiniChatModelPolicyPluginError> {
        unimplemented!("not needed in outbox tests")
    }

    async fn get_user_limits(
        &self,
        _user_id: Uuid,
        _policy_version: u64,
    ) -> Result<UserLimits, MiniChatModelPolicyPluginError> {
        unimplemented!("not needed in outbox tests")
    }

    async fn check_user_license(
        &self,
        _user_id: Uuid,
    ) -> Result<mini_chat_sdk::UserLicenseStatus, MiniChatModelPolicyPluginError> {
        unimplemented!("not needed in outbox tests")
    }

    async fn publish_usage(&self, _payload: UsageEvent) -> Result<(), PublishError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let result = {
            let guard = self.result.lock().unwrap();
            match &*guard {
                Ok(()) => Ok(()),
                Err(PublishError::Transient(r)) => Err(PublishError::Transient(r.clone())),
                Err(PublishError::Permanent(r)) => Err(PublishError::Permanent(r.clone())),
            }
        };
        self.notifier.notify_one();
        result
    }
}

/// Wraps a mock plugin as a [`PolicyPluginProvider`] for tests.
struct MockProvider {
    plugin: Arc<dyn MiniChatModelPolicyPluginClientV1>,
}

#[async_trait]
impl PolicyPluginProvider for MockProvider {
    async fn get_plugin(
        &self,
    ) -> Result<Arc<dyn MiniChatModelPolicyPluginClientV1>, crate::domain::error::DomainError> {
        Ok(self.plugin.clone())
    }
}

fn make_handler(plugin: &Arc<dyn MiniChatModelPolicyPluginClientV1>) -> UsageEventHandler {
    UsageEventHandler {
        plugin_provider: Arc::new(MockProvider {
            plugin: plugin.clone(),
        }),
    }
}

// ── 7.1: partition_for returns values in [0, num_partitions) ──

#[test]
fn partition_for_returns_in_range() {
    for num_partitions in [1u32, 2, 4, 8, 16, 32, 64] {
        for _ in 0..100 {
            let tenant_id = Uuid::new_v4();
            let p = InfraOutboxEnqueuer::compute_partition(tenant_id, num_partitions);
            assert!(
                p < num_partitions,
                "partition {p} >= num_partitions {num_partitions} for tenant {tenant_id}"
            );
        }
    }
}

#[test]
fn partition_for_deterministic() {
    let tenant_id = Uuid::new_v4();
    let a = InfraOutboxEnqueuer::compute_partition(tenant_id, 4);
    let b = InfraOutboxEnqueuer::compute_partition(tenant_id, 4);
    assert_eq!(a, b);
}

// ── 7.2 / 7.7: UsageEventHandler returns Ok when plugin returns Ok ──

#[tokio::test]
async fn handler_success_for_valid_event() {
    let plugin = MockPlugin::ok();
    let handler = make_handler(&(plugin.clone() as Arc<dyn MiniChatModelPolicyPluginClientV1>));
    let event = make_usage_event();
    let payload = serde_json::to_vec(&event).unwrap();
    let msg = make_outbox_message(payload);

    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    assert!(matches!(result, MessageResult::Ok));
    assert_eq!(plugin.calls(), 1);
}

// ── 7.3: UsageEventHandler returns Reject for invalid payload ──

#[tokio::test]
async fn handler_reject_for_invalid_payload() {
    let plugin = MockPlugin::ok();
    let handler = make_handler(&(plugin.clone() as Arc<dyn MiniChatModelPolicyPluginClientV1>));
    let msg = make_outbox_message(b"not json".to_vec());

    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    match result {
        MessageResult::Reject(reason) => {
            assert!(
                reason.contains("deserialization failed"),
                "unexpected reason: {reason}"
            );
        }
        MessageResult::Ok => panic!("expected Reject, got Ok"),
        MessageResult::Retry => panic!("expected Reject, got Retry"),
    }
    // Plugin should not be called for invalid payload.
    assert_eq!(plugin.calls(), 0);
}

// ── 7.8: UsageEventHandler returns Retry on PublishError::Transient ──

#[tokio::test]
async fn handler_retry_on_transient_error() {
    let plugin = MockPlugin::transient("network timeout");
    let handler = make_handler(&(plugin.clone() as Arc<dyn MiniChatModelPolicyPluginClientV1>));
    let event = make_usage_event();
    let payload = serde_json::to_vec(&event).unwrap();
    let msg = make_outbox_message(payload);

    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    assert!(
        matches!(result, MessageResult::Retry),
        "expected Retry, got {result:?}"
    );
    assert_eq!(plugin.calls(), 1);
}

// ── 7.9: UsageEventHandler returns Reject on PublishError::Permanent ──

#[tokio::test]
async fn handler_reject_on_permanent_error() {
    let plugin = MockPlugin::permanent("schema mismatch");
    let handler = make_handler(&(plugin.clone() as Arc<dyn MiniChatModelPolicyPluginClientV1>));
    let event = make_usage_event();
    let payload = serde_json::to_vec(&event).unwrap();
    let msg = make_outbox_message(payload);

    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    match result {
        MessageResult::Reject(reason) => {
            assert_eq!(reason, "schema mismatch");
        }
        MessageResult::Ok => panic!("expected Reject, got Ok"),
        MessageResult::Retry => panic!("expected Reject, got Retry"),
    }
    assert_eq!(plugin.calls(), 1);
}

// ── AuditEventHandler unit tests ──

/// Mock audit plugin that records `emit_*` calls and always returns `Ok(())`.
enum AuditBehavior {
    Ok,
    Transient(String),
    Permanent(String),
}

struct MockAuditPlugin {
    call_count: AtomicU32,
    notifier: tokio::sync::Notify,
    behavior: AuditBehavior,
}

impl MockAuditPlugin {
    fn ok() -> Arc<Self> {
        Arc::new(Self {
            call_count: AtomicU32::new(0),
            notifier: tokio::sync::Notify::new(),
            behavior: AuditBehavior::Ok,
        })
    }

    fn transient(msg: &str) -> Arc<Self> {
        Arc::new(Self {
            call_count: AtomicU32::new(0),
            notifier: tokio::sync::Notify::new(),
            behavior: AuditBehavior::Transient(msg.to_owned()),
        })
    }

    fn permanent(msg: &str) -> Arc<Self> {
        Arc::new(Self {
            call_count: AtomicU32::new(0),
            notifier: tokio::sync::Notify::new(),
            behavior: AuditBehavior::Permanent(msg.to_owned()),
        })
    }

    fn calls(&self) -> u32 {
        self.call_count.load(Ordering::SeqCst)
    }

    fn record(&self) {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.notifier.notify_one();
    }

    fn emit_result(&self) -> Result<(), MiniChatAuditPluginError> {
        match &self.behavior {
            AuditBehavior::Ok => Ok(()),
            AuditBehavior::Transient(msg) => Err(MiniChatAuditPluginError::Transient(msg.clone())),
            AuditBehavior::Permanent(msg) => Err(MiniChatAuditPluginError::Permanent(msg.clone())),
        }
    }
}

#[async_trait]
impl MiniChatAuditPluginClientV1 for MockAuditPlugin {
    async fn emit_turn_audit(&self, _: TurnAuditEvent) -> Result<(), MiniChatAuditPluginError> {
        self.record();
        self.emit_result()
    }
    async fn emit_turn_retry_audit(
        &self,
        _: TurnRetryAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError> {
        self.record();
        self.emit_result()
    }
    async fn emit_turn_edit_audit(
        &self,
        _: TurnEditAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError> {
        self.record();
        self.emit_result()
    }
    async fn emit_turn_delete_audit(
        &self,
        _: TurnDeleteAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError> {
        self.record();
        self.emit_result()
    }
}

fn make_audit_envelope_payload() -> Vec<u8> {
    use mini_chat_sdk::{RequesterType, TurnAuditEventType};
    let event = AuditEnvelope::Turn(TurnAuditEvent {
        event_type: TurnAuditEventType::TurnCompleted,
        timestamp: OffsetDateTime::now_utc(),
        tenant_id: Uuid::new_v4(),
        requester_type: RequesterType::User,
        trace_id: None,
        user_id: Uuid::new_v4(),
        chat_id: Uuid::new_v4(),
        turn_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        selected_model: "gpt-4o".to_owned(),
        effective_model: "gpt-4o".to_owned(),
        policy_version_applied: None,
        usage: mini_chat_sdk::AuditUsageTokens {
            input_tokens: 10,
            output_tokens: 20,
            model: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            reasoning_tokens: None,
        },
        latency_ms: mini_chat_sdk::LatencyMs::default(),
        policy_decisions: mini_chat_sdk::PolicyDecisions {
            license: None,
            quota: mini_chat_sdk::QuotaDecision {
                decision: "allowed".to_owned(),
                quota_scope: None,
                downgrade_from: None,
                downgrade_reason: None,
            },
        },
        error_code: None,
        prompt: None,
        response: None,
        attachments: vec![],
        tool_calls: None,
    });
    serde_json::to_vec(&event).unwrap()
}

// ── AuditEventHandler: invalid payload → Reject ──
//
// Note: the handler only deserializes payloads when a plugin is present.
// Use an `ok` plugin so the handler reaches the deserialization step.

#[tokio::test]
async fn audit_handler_reject_for_invalid_payload() {
    let plugin = MockAuditPlugin::ok();
    let handler = AuditEventHandler {
        audit_gateway: AuditGateway::from_plugin(plugin),
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    };
    let msg = make_outbox_message(b"not json".to_vec());
    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    assert!(
        matches!(result, MessageResult::Reject(_)),
        "expected Reject for corrupt payload"
    );
}

// ── AuditEventHandler: no plugin configured → Ok ──

#[tokio::test]
async fn audit_handler_success_when_no_plugin_configured() {
    let handler = AuditEventHandler {
        audit_gateway: AuditGateway::noop(),
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    };
    let payload = make_audit_envelope_payload();
    let msg = make_outbox_message(payload);
    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    assert!(
        matches!(result, MessageResult::Ok),
        "expected Ok when no plugin configured"
    );
}

// ── AuditEventHandler: transient plugin error → Retry ──

#[tokio::test]
async fn audit_handler_retry_on_transient_plugin_error() {
    let plugin = MockAuditPlugin::transient("network blip");
    let audit_gateway = AuditGateway::from_plugin(plugin.clone());
    let handler = AuditEventHandler {
        audit_gateway,
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    };
    let msg = make_outbox_message(make_audit_envelope_payload());
    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    assert!(
        matches!(result, MessageResult::Retry),
        "expected Retry for transient plugin error"
    );
    assert_eq!(plugin.calls(), 1);
}

// ── AuditEventHandler: permanent plugin error → Reject ──

#[tokio::test]
async fn audit_handler_reject_on_permanent_plugin_error() {
    let plugin = MockAuditPlugin::permanent("schema mismatch");
    let audit_gateway = AuditGateway::from_plugin(plugin.clone());
    let handler = AuditEventHandler {
        audit_gateway,
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    };
    let msg = make_outbox_message(make_audit_envelope_payload());
    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    assert!(
        matches!(result, MessageResult::Reject(_)),
        "expected Reject for permanent plugin error"
    );
    assert_eq!(plugin.calls(), 1);
}

// ── 7.11: Integration test - AuditEventHandler full pipeline ──

#[tokio::test]
async fn audit_pipeline_enqueue_and_deliver() {
    use modkit::client_hub::{ClientHub, ClientScope};
    use modkit::plugins::GtsPluginSelector;
    use modkit_db::outbox::{Outbox, Partitions};
    use modkit_db::{ConnectOpts, connect_db, migration_runner::run_migrations_for_testing};
    use std::time::Duration;

    let plugin = MockAuditPlugin::ok();

    let db = connect_db(
        "sqlite:file:audit_outbox_integration?mode=memory&cache=shared",
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    run_migrations_for_testing(&db, modkit_db::outbox::outbox_migrations())
        .await
        .expect("outbox migrations");

    // Build an AuditGateway backed by the mock plugin.
    // Pre-warm the selector with the instance ID and register the mock
    // directly in the ClientHub to bypass GTS types-registry resolution.
    let instance_id = "test.audit.plugin.v1~test._.recording.v1";
    let hub = Arc::new(ClientHub::new());
    hub.register_scoped::<dyn MiniChatAuditPluginClientV1>(
        ClientScope::gts_id(instance_id),
        plugin.clone() as Arc<dyn MiniChatAuditPluginClientV1>,
    );
    let selector = GtsPluginSelector::new();
    selector
        .get_or_init(|| async { Ok::<_, anyhow::Error>(instance_id.to_owned()) })
        .await
        .expect("pre-warm selector");
    let audit_gateway =
        crate::infra::audit_gateway::AuditGateway::new_preconfigured(hub, String::new(), selector);

    let handle = Outbox::builder(db.clone())
        .queue("test.audit", Partitions::of(1))
        .leased(AuditEventHandler {
            audit_gateway: Arc::clone(&audit_gateway),
            metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
        })
        .start()
        .await
        .expect("outbox start");

    let enqueuer = InfraOutboxEnqueuer::new(
        "test.usage".to_owned(),
        "test.cleanup".to_owned(),
        "test.chat_cleanup".to_owned(),
        "test.thread_summary".to_owned(),
        "test.audit".to_owned(),
        1u32,
    );
    enqueuer.set_outbox(Arc::clone(handle.outbox()));

    let payload = make_audit_envelope_payload();
    let envelope: AuditEnvelope = serde_json::from_slice(&payload).unwrap();
    let conn = db.conn().expect("conn");
    enqueuer
        .enqueue_audit_event(&conn, envelope)
        .await
        .expect("enqueue");
    enqueuer.flush();

    tokio::time::timeout(Duration::from_secs(5), plugin.notifier.notified())
        .await
        .expect("audit plugin should have been called within 5s");

    assert_eq!(
        plugin.calls(),
        1,
        "emit_turn_audit should have been called once"
    );

    handle.stop().await;
}

// ── 7.5 / 7.10: Integration test - full pipeline with mock plugin ──

#[tokio::test]
async fn full_pipeline_enqueue_and_deliver() {
    use modkit_db::outbox::{Outbox, Partitions};
    use modkit_db::{ConnectOpts, connect_db, migration_runner::run_migrations_for_testing};
    use std::time::Duration;

    // Mock plugin that tracks calls.
    let plugin = MockPlugin::ok();

    // Set up in-memory DB with outbox migrations.
    let db = connect_db(
        "sqlite:file:outbox_integration?mode=memory&cache=shared",
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    run_migrations_for_testing(&db, modkit_db::outbox::outbox_migrations())
        .await
        .expect("outbox migrations");

    // Start outbox pipeline with the real UsageEventHandler + mock plugin.
    let handle = Outbox::builder(db.clone())
        .queue("test.usage", Partitions::of(1))
        .leased(UsageEventHandler {
            plugin_provider: Arc::new(MockProvider {
                plugin: plugin.clone(),
            }),
        })
        .start()
        .await
        .expect("outbox start");

    // Enqueue a usage event using InfraOutboxEnqueuer.
    let enqueuer = InfraOutboxEnqueuer::new(
        "test.usage".to_owned(),
        "test.cleanup".to_owned(),
        "test.chat_cleanup".to_owned(),
        "test.thread_summary".to_owned(),
        "test.audit".to_owned(),
        1u32,
    );
    enqueuer.set_outbox(Arc::clone(handle.outbox()));
    let event = make_usage_event();
    let conn = db.conn().expect("conn");
    enqueuer
        .enqueue_usage_event(&conn, event)
        .await
        .expect("enqueue");
    enqueuer.flush();

    // Wait for the handler to process (notification-based, no fixed sleep).
    tokio::time::timeout(Duration::from_secs(5), plugin.notifier.notified())
        .await
        .expect("plugin should have been called within 5s");

    assert_eq!(
        plugin.calls(),
        1,
        "publish_usage should have been called once"
    );

    handle.stop().await;
}
