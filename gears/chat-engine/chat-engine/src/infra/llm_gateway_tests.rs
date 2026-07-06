use super::*;
use chat_engine_sdk::models::{Message, MessageRole, TenantId, UserId};
use futures::stream;
use std::sync::Mutex;
use time::OffsetDateTime;

// --------------------------------------------------------------- fakes -

#[derive(Default)]
struct FakeLlmGatewayClient {
    events: Mutex<Vec<UpstreamEvent>>,
    // Captures the most recent request payload so tests can introspect
    // the assembled `LlmGatewayRequest` if needed. Read access is not
    // exercised in the current test suite but the field is part of
    // the fake's contract for future tests.
    #[allow(dead_code)]
    last_request: Mutex<Option<LlmGatewayRequest>>,
    summary: Mutex<Option<String>>,
}

impl FakeLlmGatewayClient {
    fn with_events(events: Vec<UpstreamEvent>) -> Self {
        Self {
            events: Mutex::new(events),
            ..Default::default()
        }
    }
}

#[async_trait]
impl LlmGatewayClient for FakeLlmGatewayClient {
    async fn stream_chat(
        &self,
        _config: &LlmPluginConfig,
        request: LlmGatewayRequest,
    ) -> Result<UpstreamStream, PluginError> {
        *self.last_request.lock().unwrap() = Some(request);
        // Collect under the lock so the guard is released before the
        // stream is built (the stream can't borrow the guard).
        #[allow(clippy::needless_collect)]
        let events: Vec<UpstreamEvent> = self.events.lock().unwrap().drain(..).collect();
        let s = stream::iter(events.into_iter().map(Ok));
        Ok(s.boxed())
    }

    async fn summarize(
        &self,
        _config: &LlmPluginConfig,
        _messages: Vec<Message>,
    ) -> Result<String, PluginError> {
        Ok(self
            .summary
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "<empty>".into()))
    }
}

#[derive(Default)]
struct FakeModelRegistry {
    models: Vec<String>,
    default_model: String,
    per_model_caps: Vec<ModelCapabilitySchema>,
}

impl FakeModelRegistry {
    fn new() -> Self {
        Self {
            models: vec!["gpt-4".into(), "gpt-3.5".into()],
            default_model: "gpt-4".into(),
            per_model_caps: vec![ModelCapabilitySchema {
                name: "max_tokens".into(),
                schema: serde_json::json!({
                    "type": "int",
                    "min": 1,
                    "max": 4096,
                    "default_value": 1024,
                }),
            }],
        }
    }
}

#[async_trait]
impl ModelRegistryClient for FakeModelRegistry {
    async fn list_models(&self, _config: &LlmPluginConfig) -> Result<ModelCatalog, PluginError> {
        Ok(ModelCatalog {
            model_ids: self.models.clone(),
            default_model_id: self.default_model.clone(),
        })
    }

    async fn model_capabilities(
        &self,
        _config: &LlmPluginConfig,
        _model_id: &str,
    ) -> Result<Vec<ModelCapabilitySchema>, PluginError> {
        Ok(self.per_model_caps.clone())
    }
}

// -------------------------------------------------------------- helpers -

fn make_message(role: MessageRole, text: &str) -> Message {
    let now = OffsetDateTime::now_utc();
    Message {
        message_id: Uuid::new_v4(),
        session_id: Uuid::nil(),
        tenant_id: None,
        user_id: None,
        parent_message_id: None,
        variant_index: 0,
        is_active: true,
        role,
        parts: vec![chat_engine_sdk::models::MessagePart::text(
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
            0,
            text,
        )],
        file_ids: vec![],
        metadata: None,
        is_complete: true,
        is_hidden_from_user: false,
        is_hidden_from_backend: false,
        created_at: now,
        updated_at: now,
    }
}

fn make_call_ctx(config: serde_json::Value) -> PluginCallContext {
    PluginCallContext {
        request_id: Uuid::nil(),
        tenant_id: TenantId::new("t"),
        user_id: UserId::new("u"),
        plugin_instance_id: LLM_GATEWAY_PLUGIN_INSTANCE_ID.into(),
        session_type_id: Uuid::nil(),
        plugin_config: Some(config),
        enabled_capabilities: None,
        deadline: None,
        cancel: CancellationToken::new(),
    }
}

fn make_plugin(client: FakeLlmGatewayClient) -> LlmGatewayPlugin {
    LlmGatewayPlugin::new(Arc::new(client), Arc::new(FakeModelRegistry::new()))
}

fn valid_config() -> serde_json::Value {
    serde_json::json!({
        "gateway_url": "https://gw.example",
        "summarization_settings": { "recent_messages_to_keep": 4 },
    })
}

// ---------------------------------------------------------------- tests -

#[tokio::test]
async fn plugin_instance_id_matches_adr_constant() {
    let plugin = make_plugin(FakeLlmGatewayClient::default());
    assert_eq!(
        plugin.plugin_instance_id(),
        "gtx.cf.chat_engine.llm_gateway_plugin.v1~",
    );
}

#[tokio::test]
async fn on_session_type_configured_validates_and_returns_empty_caps() {
    let plugin = make_plugin(FakeLlmGatewayClient::default());
    let ctx = SessionPluginCtx {
        session_type_id: Uuid::nil(),
        session_id: None,
        call_ctx: make_call_ctx(valid_config()),
    };
    let caps = plugin.on_session_type_configured(ctx).await.unwrap();
    assert!(caps.capabilities.is_empty());
}

#[tokio::test]
async fn on_session_type_configured_rejects_invalid_config() {
    let plugin = make_plugin(FakeLlmGatewayClient::default());
    let ctx = SessionPluginCtx {
        session_type_id: Uuid::nil(),
        session_id: None,
        call_ctx: make_call_ctx(serde_json::json!({ "gateway_url": "" })),
    };
    let err = plugin
        .on_session_type_configured(ctx)
        .await
        .expect_err("empty gateway_url must be rejected");
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[tokio::test]
async fn on_session_created_emits_three_baseline_capabilities() {
    let plugin = make_plugin(FakeLlmGatewayClient::default());
    let ctx = SessionPluginCtx {
        session_type_id: Uuid::nil(),
        session_id: Some(Uuid::new_v4()),
        call_ctx: make_call_ctx(valid_config()),
    };
    let caps = plugin.on_session_created(ctx).await.unwrap();
    let names: Vec<&str> = caps.capabilities.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&CAPABILITY_MODEL));
    assert!(names.contains(&CAPABILITY_TEMPERATURE));
    assert!(names.contains(&CAPABILITY_STREAM));
    // model-specific extension from the registry must also surface.
    assert!(names.contains(&"max_tokens"));
}

#[tokio::test]
async fn on_message_streams_chunk_then_metadata() {
    let meta = LlmMessageMetadata {
        model_used: "gpt-4".into(),
        finish_reason: crate::domain::llm_config::FinishReason::Stop,
        temperature_used: Some(0.7),
        usage: Some(crate::domain::llm_config::LlmUsage {
            prompt_tokens: 3,
            completion_tokens: 5,
            total_tokens: 8,
            cached_tokens: None,
        }),
    };
    let client = FakeLlmGatewayClient::with_events(vec![
        UpstreamEvent::Chunk("hello ".into()),
        UpstreamEvent::Chunk("world".into()),
        UpstreamEvent::Complete(meta.clone()),
    ]);
    let plugin = make_plugin(client);
    let ctx = MessagePluginCtx {
        session_id: Uuid::new_v4(),
        message_id: Uuid::new_v4(),
        messages: vec![make_message(MessageRole::User, "hi")],
        call_ctx: make_call_ctx(valid_config()),
    };
    let mut stream = plugin.on_message(ctx).await.unwrap();
    let mut events: Vec<StreamingEvent> = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.unwrap());
    }
    // two chunks + one complete
    assert_eq!(events.len(), 3, "got {events:?}");
    assert!(matches!(events[0], StreamingEvent::Chunk(_)));
    assert!(matches!(events[1], StreamingEvent::Chunk(_)));
    match &events[2] {
        StreamingEvent::Complete(c) => {
            let metadata_json = c.metadata.as_ref().expect("metadata present");
            let parsed: LlmMessageMetadata = serde_json::from_value(metadata_json.clone()).unwrap();
            assert_eq!(parsed, meta);
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[tokio::test]
async fn context_overflow_surfaces_as_discriminator_prefixed_error() {
    let client = FakeLlmGatewayClient::with_events(vec![UpstreamEvent::ContextOverflow(
        "window exceeded by 200 tokens".into(),
    )]);
    let plugin = make_plugin(client);
    let msg_id = Uuid::new_v4();
    let ctx = MessagePluginCtx {
        session_id: Uuid::new_v4(),
        message_id: msg_id,
        messages: vec![make_message(MessageRole::User, "very long prompt")],
        call_ctx: make_call_ctx(valid_config()),
    };
    let mut stream = plugin.on_message(ctx).await.unwrap();
    let event = stream.next().await.expect("one event").unwrap();
    match event {
        StreamingEvent::Error(e) => {
            assert_eq!(e.message_id, msg_id);
            assert!(
                e.error.starts_with(ERROR_PREFIX_CONTEXT_OVERFLOW),
                "got: {}",
                e.error
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert!(
        stream.next().await.is_none(),
        "stream must close after overflow"
    );
}

#[tokio::test]
async fn stream_interrupted_surfaces_as_discriminator_prefixed_error() {
    let client = FakeLlmGatewayClient::with_events(vec![
        UpstreamEvent::Chunk("partial".into()),
        UpstreamEvent::StreamInterrupted("upstream RST".into()),
    ]);
    let plugin = make_plugin(client);
    let msg_id = Uuid::new_v4();
    let ctx = MessagePluginCtx {
        session_id: Uuid::new_v4(),
        message_id: msg_id,
        messages: vec![make_message(MessageRole::User, "hi")],
        call_ctx: make_call_ctx(valid_config()),
    };
    let mut stream = plugin.on_message(ctx).await.unwrap();
    let _chunk = stream.next().await.unwrap().unwrap();
    let event = stream.next().await.unwrap().unwrap();
    match event {
        StreamingEvent::Error(e) => {
            assert!(
                e.error.starts_with(ERROR_PREFIX_STREAM_INTERRUPTED),
                "got: {}",
                e.error
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn on_message_rejects_hidden_from_backend_input() {
    let plugin = make_plugin(FakeLlmGatewayClient::default());
    let mut hidden = make_message(MessageRole::User, "stale");
    hidden.is_hidden_from_backend = true;
    let ctx = MessagePluginCtx {
        session_id: Uuid::new_v4(),
        message_id: Uuid::new_v4(),
        messages: vec![hidden],
        call_ctx: make_call_ctx(valid_config()),
    };
    let result = plugin.on_message(ctx).await;
    let err = match result {
        Ok(_) => panic!("hidden-from-backend message must be rejected"),
        Err(e) => e,
    };
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[tokio::test]
async fn on_message_emits_deadline_exceeded_when_remaining_zero() {
    let plugin = make_plugin(FakeLlmGatewayClient::default());
    let mut call_ctx = make_call_ctx(valid_config());
    // Deadline already elapsed — Some(ZERO) by construction.
    call_ctx.deadline = Some(
        Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap(),
    );
    let msg_id = Uuid::new_v4();
    let ctx = MessagePluginCtx {
        session_id: Uuid::new_v4(),
        message_id: msg_id,
        messages: vec![make_message(MessageRole::User, "hi")],
        call_ctx,
    };
    let mut stream = plugin.on_message(ctx).await.unwrap();
    let event = stream.next().await.unwrap().unwrap();
    match event {
        StreamingEvent::Error(e) => {
            assert_eq!(e.message_id, msg_id);
            assert!(
                e.error.starts_with(ERROR_PREFIX_DEADLINE_EXCEEDED),
                "got: {}",
                e.error
            );
        }
        other => panic!("expected deadline-exceeded Error, got {other:?}"),
    }
}

#[tokio::test]
async fn on_session_summary_requires_summarization_settings() {
    let plugin = make_plugin(FakeLlmGatewayClient::default());
    let ctx = SessionPluginCtx {
        session_type_id: Uuid::nil(),
        session_id: Some(Uuid::new_v4()),
        // Note: no `summarization_settings` field — should produce
        // PluginError::Internal per the unsupported rule.
        call_ctx: make_call_ctx(serde_json::json!({
            "gateway_url": "https://gw.example",
        })),
    };
    let result = plugin.on_session_summary(ctx).await;
    let err = match result {
        Ok(_) => panic!("missing summarization_settings must surface"),
        Err(e) => e,
    };
    assert!(matches!(err, PluginError::Internal { .. }));
}
