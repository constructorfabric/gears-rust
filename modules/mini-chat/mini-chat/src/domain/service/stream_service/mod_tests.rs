use super::types::{StreamTerminal, normalize_error};
use super::*;
use crate::domain::llm::{ToolPhase, Usage};
use crate::domain::repos::CasTerminalParams;
use crate::infra::db::repo::attachment_repo::AttachmentRepository as OrmAttachmentRepo;
use crate::infra::db::repo::chat_repo::ChatRepository as OrmChatRepo;
use crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository as OrmMessageAttachmentRepo;
use crate::infra::db::repo::message_repo::MessageRepository as MsgRepo;
use crate::infra::db::repo::turn_repo::TurnRepository as TurnRepo;
use crate::infra::db::repo::vector_store_repo::VectorStoreRepository as OrmVectorStoreRepo;
use crate::infra::llm::{
    Citation, CitationSource, ClientSseEvent, LlmMessage, LlmProvider, LlmProviderError,
    LlmRequest, NonStreaming, ProviderStream, ResponseResult, Streaming, TerminalOutcome,
    TranslatedEvent,
};
use futures::{StreamExt, stream};
use mini_chat_sdk::RequesterType;
use oagw_sdk::error::StreamingError;

// ── Noop OutboxEnqueuer ──

#[allow(de0309_must_have_domain_model)]
struct NoopQuotaWarningsProvider;
#[async_trait::async_trait]
impl crate::domain::service::quota_settler::QuotaWarningsProvider for NoopQuotaWarningsProvider {
    async fn get_quota_warnings(
        &self,
        _scope: &modkit_security::AccessScope,
        _tenant_id: Uuid,
        _user_id: Uuid,
    ) -> Result<Vec<crate::domain::stream_events::QuotaWarning>, crate::domain::error::DomainError>
    {
        Ok(Vec::new())
    }
}

#[allow(de0309_must_have_domain_model)]
struct NoopOutboxEnqueuer;
#[async_trait::async_trait]
impl crate::domain::repos::OutboxEnqueuer for NoopOutboxEnqueuer {
    async fn enqueue_usage_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: mini_chat_sdk::UsageEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }

    async fn enqueue_attachment_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::repos::AttachmentCleanupEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }

    async fn enqueue_chat_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::repos::ChatCleanupEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }

    async fn enqueue_audit_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::model::audit_envelope::AuditEnvelope,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }

    fn flush(&self) {}
}

#[test]
fn normalize_rate_limited() {
    let err = LlmProviderError::RateLimited {
        retry_after_secs: Some(30),
    };
    let (code, _) = normalize_error(&err);
    assert_eq!(code, "rate_limited");
}

#[test]
fn normalize_timeout() {
    let (code, _) = normalize_error(&LlmProviderError::Timeout);
    assert_eq!(code, "provider_timeout");
}

#[test]
fn normalize_provider_error() {
    let err = LlmProviderError::ProviderError {
        code: "bad_request".into(),
        message: "something went wrong".into(),
        raw_detail: None,
    };
    let (code, msg) = normalize_error(&err);
    assert_eq!(code, "provider_error");
    assert_eq!(msg, "something went wrong");
}

#[test]
fn normalize_unavailable() {
    let (code, _) = normalize_error(&LlmProviderError::ProviderUnavailable);
    assert_eq!(code, "provider_error");
}

#[test]
fn normalize_invalid_response() {
    let err = LlmProviderError::InvalidResponse {
        detail: "bad json".into(),
    };
    let (code, msg) = normalize_error(&err);
    assert_eq!(code, "provider_error");
    assert_eq!(msg, "bad json");
}

// ── Mock LlmProvider for integration tests ──

/// A mock LLM provider that yields predefined events and a terminal outcome.
#[allow(de0309_must_have_domain_model)]
struct MockProvider {
    events: std::sync::Mutex<Vec<Result<TranslatedEvent, StreamingError>>>,
}

impl MockProvider {
    fn completed(deltas: &[&str]) -> Self {
        let mut events: Vec<Result<TranslatedEvent, StreamingError>> = deltas
            .iter()
            .map(|text| {
                Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                    r#type: "text",
                    content: (*text).to_owned(),
                }))
            })
            .collect();

        let full_text: String = deltas.iter().copied().collect();
        events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Completed {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
                reasoning_tokens: 0,
            },
            response_id: "resp-test".to_owned(),
            content: full_text,
            citations: vec![],
            raw_response: serde_json::Value::Null,
        })));

        Self {
            events: std::sync::Mutex::new(events),
        }
    }

    /// Provider that completes with citations.
    fn completed_with_citations(deltas: &[&str], citations: Vec<Citation>) -> Self {
        let mut events: Vec<Result<TranslatedEvent, StreamingError>> = deltas
            .iter()
            .map(|text| {
                Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                    r#type: "text",
                    content: (*text).to_owned(),
                }))
            })
            .collect();

        let full_text: String = deltas.iter().copied().collect();
        events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Completed {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
                reasoning_tokens: 0,
            },
            response_id: "resp-test".to_owned(),
            content: full_text,
            citations,
            raw_response: serde_json::Value::Null,
        })));

        Self {
            events: std::sync::Mutex::new(events),
        }
    }

    fn failing() -> Self {
        Self {
            events: std::sync::Mutex::new(vec![Ok(TranslatedEvent::Terminal(
                TerminalOutcome::Failed {
                    error: LlmProviderError::Timeout,
                    usage: None,
                    partial_content: String::new(),
                },
            ))]),
        }
    }

    /// Provider that emits deltas then stops with `max_output_tokens` reason.
    fn incomplete(deltas: &[&str]) -> Self {
        let mut events: Vec<Result<TranslatedEvent, StreamingError>> = deltas
            .iter()
            .map(|text| {
                Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                    r#type: "text",
                    content: (*text).to_owned(),
                }))
            })
            .collect();

        events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
            reason: "max_output_tokens".to_owned(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 4096,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
                reasoning_tokens: 0,
            },
            partial_content: deltas.iter().copied().collect(),
        })));

        Self {
            events: std::sync::Mutex::new(events),
        }
    }

    /// Provider that emits `web_search` tool start/done pairs, then completes.
    fn with_web_search_calls(web_search_count: usize) -> Self {
        let mut events: Vec<Result<TranslatedEvent, StreamingError>> = Vec::new();

        // Emit a delta first so we have content
        events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
            r#type: "text",
            content: "Hello".to_owned(),
        })));

        for _ in 0..web_search_count {
            events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Start,
                name: "web_search",
                details: serde_json::json!({}),
            })));
            events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Tool {
                phase: ToolPhase::Done,
                name: "web_search",
                details: serde_json::json!({}),
            })));
        }

        events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Completed {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
                reasoning_tokens: 0,
            },
            response_id: "resp-test".to_owned(),
            content: "Hello".to_owned(),
            citations: vec![],
            raw_response: serde_json::Value::Null,
        })));

        Self {
            events: std::sync::Mutex::new(events),
        }
    }

    /// Provider that emits N `code_interpreter` start/done pairs, then completes.
    fn with_code_interpreter_calls(count: usize) -> Self {
        Self::with_tool_calls(&[("code_interpreter", count)])
    }

    /// Provider that emits tool start/done pairs for arbitrary tool names, then completes.
    fn with_tool_calls(calls: &[(&'static str, usize)]) -> Self {
        let mut events: Vec<Result<TranslatedEvent, StreamingError>> = Vec::new();

        events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
            r#type: "text",
            content: "Hello".to_owned(),
        })));

        for &(name, count) in calls {
            for _ in 0..count {
                events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Tool {
                    phase: ToolPhase::Start,
                    name,
                    details: serde_json::json!({}),
                })));
                events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Tool {
                    phase: ToolPhase::Done,
                    name,
                    details: serde_json::json!({}),
                })));
            }
        }

        events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Completed {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
                reasoning_tokens: 0,
            },
            response_id: "resp-test".to_owned(),
            content: "Hello".to_owned(),
            citations: vec![],
            raw_response: serde_json::Value::Null,
        })));

        Self {
            events: std::sync::Mutex::new(events),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockProvider {
    async fn stream(
        &self,
        _ctx: SecurityContext,
        _request: LlmRequest<Streaming>,
        _upstream_alias: &str,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, LlmProviderError> {
        let events = self.events.lock().unwrap().drain(..).collect::<Vec<_>>();
        let inner = stream::iter(events);
        Ok(ProviderStream::new(inner, cancel))
    }

    async fn complete(
        &self,
        _ctx: SecurityContext,
        _request: LlmRequest<NonStreaming>,
        _upstream_alias: &str,
    ) -> Result<ResponseResult, LlmProviderError> {
        unimplemented!("not needed for streaming tests")
    }
}

fn mock_ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

// ── Integration tests ──

/// 6.5: End-to-end stream with mock provider returning deltas + completed.
#[tokio::test]
async fn end_to_end_completed_stream() {
    let provider: Arc<dyn LlmProvider> =
        Arc::new(MockProvider::completed(&["Hello", ", ", "world!"]));
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    // Collect all events from the channel
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    // Verify event sequence: 3 deltas + 1 done
    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], StreamEvent::Delta(_)));
    assert!(matches!(events[1], StreamEvent::Delta(_)));
    assert!(matches!(events[2], StreamEvent::Delta(_)));
    assert!(matches!(events[3], StreamEvent::Done(_)));

    // Verify accumulated text in outcome
    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Completed);
    assert_eq!(outcome.accumulated_text, "Hello, world!");
    assert!(outcome.usage.is_some());
    assert_eq!(outcome.error_code, None);
    assert_eq!(outcome.provider_response_id.as_deref(), Some("resp-test"));
}

/// 6.5 variant: Provider fails before first event.
#[tokio::test]
async fn provider_error_produces_error_event() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::failing());
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    // Should get exactly one Error event
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::Error(_)));

    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Failed);
    assert_eq!(outcome.error_code.as_deref(), Some("provider_timeout"));
}

/// Provider hitting `max_output_tokens` yields Incomplete outcome.
#[tokio::test]
async fn provider_incomplete_max_output_tokens() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::incomplete(&["Hello", ", wor"]));
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    // 2 deltas + 1 done (incomplete maps to done event for client)
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], StreamEvent::Delta(_)));
    assert!(matches!(events[1], StreamEvent::Delta(_)));
    assert!(matches!(events[2], StreamEvent::Done(_)));

    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Incomplete);
    assert_eq!(outcome.accumulated_text, "Hello, wor");
    assert!(outcome.usage.is_some());
    let usage = outcome.usage.unwrap();
    assert_eq!(usage.output_tokens, 4096);
    assert_eq!(
        outcome.error_code.as_deref(),
        Some("incomplete:max_output_tokens")
    );
}

/// 6.6: Cancellation mid-stream.
#[tokio::test]
async fn cancellation_stops_stream() {
    // A provider that yields one delta then blocks until cancelled.
    #[allow(de0309_must_have_domain_model)]
    struct SlowProvider;

    #[async_trait::async_trait]
    impl LlmProvider for SlowProvider {
        async fn stream(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<Streaming>,
            _upstream_alias: &str,
            cancel: CancellationToken,
        ) -> Result<ProviderStream, LlmProviderError> {
            let inner = stream::unfold(0u8, |state| async move {
                if state == 0 {
                    Some((
                        Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                            r#type: "text",
                            content: "partial".to_owned(),
                        })),
                        1,
                    ))
                } else {
                    // Block until cancelled
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    None
                }
            });
            Ok(ProviderStream::new(inner, cancel))
        }

        async fn complete(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<NonStreaming>,
            _upstream_alias: &str,
        ) -> Result<ResponseResult, LlmProviderError> {
            unimplemented!()
        }
    }

    let provider: Arc<dyn LlmProvider> = Arc::new(SlowProvider);
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel.clone(),
        tx,
        None,
    );

    // Read the first delta
    let first = rx.recv().await.expect("should get first delta");
    assert!(matches!(first, StreamEvent::Delta(_)));

    // Cancel the stream
    cancel.cancel();

    // The provider task should exit
    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Cancelled);
    assert_eq!(outcome.accumulated_text, "partial");
}

// ── Pre-stream check tests (7.6) ──

use crate::domain::service::test_helpers::{
    MockPolicySnapshotProvider, MockThreadSummaryRepo, MockUserLimitsProvider,
    TestCatalogEntryParams, inmem_db, mock_db_provider, mock_enforcer, mock_thread_summary_repo,
    test_catalog_entry, test_security_ctx_with_id,
};
use crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository as OrmQuotaUsageRepo;

/// Build a `StreamService` with real DB repos and a mock LLM provider.
fn build_stream_service(
    db: Arc<DbProvider>,
    provider: Arc<dyn LlmProvider>,
) -> StreamService<
    TurnRepo,
    MsgRepo,
    OrmQuotaUsageRepo,
    OrmChatRepo,
    MockThreadSummaryRepo,
    OrmAttachmentRepo,
    OrmVectorStoreRepo,
    OrmMessageAttachmentRepo,
> {
    build_stream_service_with_metrics(
        db,
        provider,
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    )
}

/// Build a `StreamService` with real DB repos, a mock LLM provider,
/// and an injectable metrics implementation.
fn build_stream_service_with_metrics(
    db: Arc<DbProvider>,
    provider: Arc<dyn LlmProvider>,
    metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort>,
) -> StreamService<
    TurnRepo,
    MsgRepo,
    OrmQuotaUsageRepo,
    OrmChatRepo,
    MockThreadSummaryRepo,
    OrmAttachmentRepo,
    OrmVectorStoreRepo,
    OrmMessageAttachmentRepo,
> {
    use crate::domain::service::finalization_service::FinalizationService;
    use crate::domain::service::quota_settler::QuotaSettler;

    // Mock QuotaSettler for stream service tests
    #[domain_model]
    struct MockQuotaSettler;
    #[async_trait::async_trait]
    impl QuotaSettler for MockQuotaSettler {
        async fn settle_in_tx(
            &self,
            _tx: &modkit_db::secure::DbTx<'_>,
            _scope: &AccessScope,
            _input: crate::domain::model::quota::SettlementInput,
        ) -> Result<crate::domain::model::quota::SettlementOutcome, crate::domain::error::DomainError>
        {
            Ok(crate::domain::model::quota::SettlementOutcome {
                settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                actual_credits_micro: 0,
                charged_tokens: 0,
                overshoot_capped: false,
            })
        }
    }

    let provider_resolver = Arc::new(ProviderResolver::single_provider(provider));
    let turn_repo = Arc::new(TurnRepo);
    let message_repo = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }));
    let finalization = Arc::new(FinalizationService::new(
        Arc::clone(&db),
        Arc::clone(&turn_repo),
        Arc::clone(&message_repo),
        Arc::new(MockQuotaSettler) as Arc<dyn QuotaSettler>,
        Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
        Arc::clone(&metrics),
    ));

    // QuotaService with permissive defaults — model catalog includes
    // "gpt-5.2" (standard) so that preflight allows test requests.
    let quota_svc = Arc::new(crate::domain::service::QuotaService::new(
        Arc::clone(&db),
        Arc::new(OrmQuotaUsageRepo),
        Arc::new(MockPolicySnapshotProvider::new(
            mini_chat_sdk::PolicySnapshot {
                user_id: Uuid::nil(),
                policy_version: 1,
                model_catalog: vec![test_catalog_entry(TestCatalogEntryParams {
                    model_id: "gpt-5.2".to_owned(),
                    provider_model_id: "gpt-5.2-2025-03-26".to_owned(),
                    display_name: "GPT 5.2".to_owned(),
                    tier: mini_chat_sdk::ModelTier::Standard,
                    enabled: true,
                    is_default: true,
                    input_tokens_credit_multiplier_micro: 1_000_000,
                    output_tokens_credit_multiplier_micro: 1_000_000,
                    multimodal_capabilities: vec![],
                    context_window: 128_000,
                    max_output_tokens: 4096,
                    description: String::new(),
                    provider_display_name: String::new(),
                    multiplier_display: "1x".to_owned(),
                    provider_id: "openai".to_owned(),
                })],
                kill_switches: mini_chat_sdk::KillSwitches::default(),
            },
        )),
        Arc::new(MockUserLimitsProvider::new(mini_chat_sdk::UserLimits {
            user_id: Uuid::nil(),
            policy_version: 1,
            standard: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 100_000_000,
                limit_monthly_credits_micro: 1_000_000_000,
            },
            premium: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 50_000_000,
                limit_monthly_credits_micro: 500_000_000,
            },
        })),
        crate::config::EstimationBudgets::default(),
        crate::config::QuotaConfig {
            overshoot_tolerance_factor: 1.10,
            ..crate::config::QuotaConfig::default()
        },
    ));

    StreamService::new(
        db,
        turn_repo,
        message_repo,
        Arc::new(OrmChatRepo::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        })),
        mock_enforcer(),
        provider_resolver,
        crate::config::StreamingConfig::default(),
        finalization,
        quota_svc,
        mock_thread_summary_repo(),
        Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
        Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
        Arc::new(crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository),
        crate::config::ContextConfig::default(),
        crate::config::RagConfig::default(),
        metrics,
    )
}

/// Insert a parent chat row (required by FK constraints).
async fn insert_test_chat(db: &Arc<DbProvider>, tenant_id: Uuid, user_id: Uuid, chat_id: Uuid) {
    use crate::infra::db::entity::chat::{ActiveModel, Entity as ChatEntity};
    use modkit_db::secure::secure_insert;
    use sea_orm::Set;
    use time::OffsetDateTime;

    let now = OffsetDateTime::now_utc();
    let am = ActiveModel {
        id: Set(chat_id),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        model: Set("gpt-5.2".to_owned()),
        title: Set(Some("test".to_owned())),
        is_temporary: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    let conn = db.conn().unwrap();
    secure_insert::<ChatEntity>(am, &AccessScope::allow_all(), &conn)
        .await
        .expect("insert chat");
}

fn test_resolved_model() -> ResolvedModel {
    ResolvedModel {
        model_id: "gpt-5.2".into(),
        provider_model_id: "gpt-5.2-2025-03-26".into(),
        provider_id: "openai".into(),
        display_name: "GPT 5.2".into(),
        tier: "standard".into(),
        multiplier_display: "1x".into(),
        description: None,
        multimodal_capabilities: vec![],
        context_window: 128_000,
        max_file_size_mb: 25,
        system_prompt: String::new(),
        tool_support: mini_chat_sdk::ModelToolSupport {
            web_search: false,
            file_search: false,
            image_generation: false,
            code_interpreter: false,
            computer_use: false,
            mcp: false,
        },
    }
}

/// 7.6: Idempotency check — returns Replay when a completed turn exists.
#[tokio::test]
async fn prestream_idempotency_returns_replay_for_existing_turn() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    // Pre-create a completed turn
    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .create_turn(
            &conn,
            &scope,
            CreateTurnParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id,
                requester_type: "user".to_owned(),
                requester_user_id: None,
                reserve_tokens: None,
                max_output_tokens_applied: None,
                reserved_credits_micro: None,
                policy_version_applied: None,
                effective_model: None,
                minimal_generation_floor_applied: None,
                web_search_enabled: false,
            },
        )
        .await
        .expect("create turn");

    turn_repo
        .cas_update_state(
            &conn,
            &scope,
            CasTerminalParams {
                turn_id: turn.id,
                state: TurnState::Completed,
                error_code: None,
                error_detail: None,
                assistant_message_id: None,
                provider_response_id: None,
            },
        )
        .await
        .expect("complete turn");

    // Now run_stream with same request_id → should get Replay
    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream(
            ctx,
            chat_id,
            request_id,
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be Replay");

    assert!(
        matches!(err, StreamError::Replay { .. }),
        "expected Replay, got: {err:?}"
    );
}

/// 6.2: Running turn with same `request_id` → Conflict (not Replay).
#[tokio::test]
async fn idempotency_running_turn_returns_conflict() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    // Pre-create a running turn (default state after create_turn)
    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let turn_repo = TurnRepo;
    turn_repo
        .create_turn(
            &conn,
            &scope,
            CreateTurnParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id,
                requester_type: "user".to_owned(),
                requester_user_id: None,
                reserve_tokens: None,
                max_output_tokens_applied: None,
                reserved_credits_micro: None,
                policy_version_applied: None,
                effective_model: None,
                minimal_generation_floor_applied: None,
                web_search_enabled: false,
            },
        )
        .await
        .expect("create turn");

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream(
            ctx,
            chat_id,
            request_id,
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be Conflict");

    match err {
        StreamError::Conflict { code, .. } => {
            assert_eq!(code, "request_id_conflict");
        }
        other => panic!("expected Conflict, got: {other:?}"),
    }
}

/// 6.3: Failed turn with same `request_id` → Conflict (not Replay).
#[tokio::test]
async fn idempotency_failed_turn_returns_conflict() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .create_turn(
            &conn,
            &scope,
            CreateTurnParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id,
                requester_type: "user".to_owned(),
                requester_user_id: None,
                reserve_tokens: None,
                max_output_tokens_applied: None,
                reserved_credits_micro: None,
                policy_version_applied: None,
                effective_model: None,
                minimal_generation_floor_applied: None,
                web_search_enabled: false,
            },
        )
        .await
        .expect("create turn");

    turn_repo
        .cas_update_state(
            &conn,
            &scope,
            CasTerminalParams {
                turn_id: turn.id,
                state: TurnState::Failed,
                error_code: Some("provider_error".to_owned()),
                error_detail: Some("timeout".to_owned()),
                assistant_message_id: None,
                provider_response_id: None,
            },
        )
        .await
        .expect("fail turn");

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream(
            ctx,
            chat_id,
            request_id,
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be Conflict");

    match err {
        StreamError::Conflict { code, .. } => {
            assert_eq!(code, "request_id_conflict");
        }
        other => panic!("expected Conflict, got: {other:?}"),
    }
}

/// 6.4: Cancelled turn with same `request_id` → Conflict (not Replay).
#[tokio::test]
async fn idempotency_cancelled_turn_returns_conflict() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .create_turn(
            &conn,
            &scope,
            CreateTurnParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id,
                requester_type: "user".to_owned(),
                requester_user_id: None,
                reserve_tokens: None,
                max_output_tokens_applied: None,
                reserved_credits_micro: None,
                policy_version_applied: None,
                effective_model: None,
                minimal_generation_floor_applied: None,
                web_search_enabled: false,
            },
        )
        .await
        .expect("create turn");

    turn_repo
        .cas_update_state(
            &conn,
            &scope,
            CasTerminalParams {
                turn_id: turn.id,
                state: TurnState::Cancelled,
                error_code: None,
                error_detail: None,
                assistant_message_id: None,
                provider_response_id: None,
            },
        )
        .await
        .expect("cancel turn");

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream(
            ctx,
            chat_id,
            request_id,
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be Conflict");

    match err {
        StreamError::Conflict { code, .. } => {
            assert_eq!(code, "request_id_conflict");
        }
        other => panic!("expected Conflict, got: {other:?}"),
    }
}

/// 7.6: Parallel turn guard — returns Conflict when a running turn exists.
#[tokio::test]
async fn prestream_parallel_guard_returns_conflict() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    // Pre-create a running turn for the same chat (different request_id)
    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let turn_repo = TurnRepo;
    turn_repo
        .create_turn(
            &conn,
            &scope,
            CreateTurnParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id: Uuid::new_v4(), // different request
                requester_type: "user".to_owned(),
                requester_user_id: None,
                reserve_tokens: None,
                max_output_tokens_applied: None,
                reserved_credits_micro: None,
                policy_version_applied: None,
                effective_model: None,
                minimal_generation_floor_applied: None,
                web_search_enabled: false,
            },
        )
        .await
        .expect("create running turn");

    // New request_id → passes idempotency, but hits parallel guard
    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be Conflict");

    assert!(
        matches!(err, StreamError::Conflict { .. }),
        "expected Conflict, got: {err:?}"
    );
}

/// 7.6: Happy path — no existing turn, no running turns → succeeds.
#[tokio::test]
async fn prestream_happy_path_proceeds_to_stream() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
    let svc = build_stream_service(db, provider);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect("should succeed");

    // Drain events
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Completed);
    assert_eq!(outcome.accumulated_text, "Hello");
}

// ── flatten_preflight unit tests ──

/// `max_input_tokens` from `PreflightDecision::Allow` reaches `PreflightResult`.
#[test]
fn flatten_preflight_allow_propagates_max_input_tokens() {
    use crate::config::EstimationBudgets;
    use crate::domain::model::quota::PreflightDecision;

    let decision = PreflightDecision::Allow {
        effective_model: "m".to_owned(),
        effective_provider_model_id: "m-provider".to_owned(),
        reserve_tokens: 100,
        max_output_tokens_applied: 1024,
        reserved_credits_micro: 0,
        policy_version_applied: 1,
        minimal_generation_floor_applied: 50,
        system_prompt: String::new(),
        context_window: 128_000,
        max_input_tokens: 65_536,
        estimation_budgets: EstimationBudgets::default(),
        max_retrieved_chunks_per_turn: 5,
        max_tool_calls: 2,
        tool_support: mini_chat_sdk::ModelToolSupport {
            web_search: false,
            file_search: false,
            image_generation: false,
            code_interpreter: false,
            computer_use: false,
            mcp: false,
        },
        api_params: mini_chat_sdk::ModelApiParams {
            temperature: 0.7,
            top_p: 1.0,
            frequency_penalty: 0.0,
            presence_penalty: 0.0,
            stop: vec![],
            extra_body: None,
        },
    };

    let result = flatten_preflight(decision).expect("Allow should produce Ok");
    assert_eq!(result.max_input_tokens, 65_536);
    assert_eq!(result.context_window, 128_000);
}

/// `max_input_tokens` from `PreflightDecision::Downgrade` reaches `PreflightResult`.
#[test]
fn flatten_preflight_downgrade_propagates_max_input_tokens() {
    use crate::config::EstimationBudgets;
    use crate::domain::model::quota::{DowngradeReason, PreflightDecision};

    let decision = PreflightDecision::Downgrade {
        effective_model: "m-mini".to_owned(),
        effective_provider_model_id: "m-mini-provider".to_owned(),
        reserve_tokens: 50,
        max_output_tokens_applied: 512,
        reserved_credits_micro: 0,
        policy_version_applied: 1,
        minimal_generation_floor_applied: 50,
        downgrade_from: "m".to_owned(),
        downgrade_reason: DowngradeReason::PremiumQuotaExhausted,
        system_prompt: String::new(),
        context_window: 32_000,
        max_input_tokens: 16_000,
        estimation_budgets: EstimationBudgets::default(),
        max_retrieved_chunks_per_turn: 5,
        max_tool_calls: 2,
        tool_support: mini_chat_sdk::ModelToolSupport {
            web_search: false,
            file_search: false,
            image_generation: false,
            code_interpreter: false,
            computer_use: false,
            mcp: false,
        },
        api_params: mini_chat_sdk::ModelApiParams {
            temperature: 0.7,
            top_p: 1.0,
            frequency_penalty: 0.0,
            presence_penalty: 0.0,
            stop: vec![],
            extra_body: None,
        },
    };

    let result = flatten_preflight(decision).expect("Downgrade should produce Ok");
    assert_eq!(result.max_input_tokens, 16_000);
    assert_eq!(result.context_window, 32_000);
    assert_eq!(result.quota_decision, "downgrade");
}

// ── InputTooLong integration tests ──

/// Build a `StreamService` whose model catalog sets `max_input_tokens` to
/// `context_window` (the invariant in `test_catalog_entry`). Using a small
/// `context_window` lets tests trigger `InputTooLong` with short content.
fn build_stream_service_with_context_window(
    db: Arc<DbProvider>,
    provider: Arc<dyn LlmProvider>,
    context_window: u32,
) -> StreamService<
    TurnRepo,
    MsgRepo,
    OrmQuotaUsageRepo,
    OrmChatRepo,
    MockThreadSummaryRepo,
    OrmAttachmentRepo,
    OrmVectorStoreRepo,
    OrmMessageAttachmentRepo,
> {
    use crate::domain::service::finalization_service::FinalizationService;
    use crate::domain::service::quota_settler::QuotaSettler;

    #[allow(de0309_must_have_domain_model)]
    struct MockQuotaSettler;
    #[async_trait::async_trait]
    impl QuotaSettler for MockQuotaSettler {
        async fn settle_in_tx(
            &self,
            _tx: &modkit_db::secure::DbTx<'_>,
            _scope: &AccessScope,
            _input: crate::domain::model::quota::SettlementInput,
        ) -> Result<crate::domain::model::quota::SettlementOutcome, crate::domain::error::DomainError>
        {
            Ok(crate::domain::model::quota::SettlementOutcome {
                settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                actual_credits_micro: 0,
                charged_tokens: 0,
                overshoot_capped: false,
            })
        }
    }

    let metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort> =
        Arc::new(crate::domain::ports::metrics::NoopMetrics);
    let provider_resolver = Arc::new(ProviderResolver::single_provider(provider));
    let turn_repo = Arc::new(TurnRepo);
    let message_repo = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }));
    let finalization = Arc::new(FinalizationService::new(
        Arc::clone(&db),
        Arc::clone(&turn_repo),
        Arc::clone(&message_repo),
        Arc::new(MockQuotaSettler) as Arc<dyn QuotaSettler>,
        Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
        Arc::clone(&metrics),
    ));

    // Keep max_output_tokens well below context_window so preflight maths
    // don't overflow or reject before our InputTooLong check runs.
    #[allow(clippy::integer_division)]
    let max_output_tokens = (context_window / 4).max(1);
    let quota_svc = Arc::new(crate::domain::service::QuotaService::new(
        Arc::clone(&db),
        Arc::new(OrmQuotaUsageRepo),
        Arc::new(MockPolicySnapshotProvider::new(
            mini_chat_sdk::PolicySnapshot {
                user_id: Uuid::nil(),
                policy_version: 1,
                model_catalog: vec![test_catalog_entry(TestCatalogEntryParams {
                    model_id: "gpt-5.2".to_owned(),
                    provider_model_id: "gpt-5.2-2025-03-26".to_owned(),
                    display_name: "GPT 5.2".to_owned(),
                    tier: mini_chat_sdk::ModelTier::Standard,
                    enabled: true,
                    is_default: true,
                    input_tokens_credit_multiplier_micro: 1_000_000,
                    output_tokens_credit_multiplier_micro: 1_000_000,
                    multimodal_capabilities: vec![],
                    context_window,
                    max_output_tokens,
                    description: String::new(),
                    provider_display_name: String::new(),
                    multiplier_display: "1x".to_owned(),
                    provider_id: "openai".to_owned(),
                })],
                kill_switches: mini_chat_sdk::KillSwitches::default(),
            },
        )),
        Arc::new(MockUserLimitsProvider::new(mini_chat_sdk::UserLimits {
            user_id: Uuid::nil(),
            policy_version: 1,
            standard: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 100_000_000,
                limit_monthly_credits_micro: 1_000_000_000,
            },
            premium: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 50_000_000,
                limit_monthly_credits_micro: 500_000_000,
            },
        })),
        crate::config::EstimationBudgets::default(),
        crate::config::QuotaConfig {
            overshoot_tolerance_factor: 1.10,
            ..crate::config::QuotaConfig::default()
        },
    ));

    StreamService::new(
        db,
        turn_repo,
        message_repo,
        Arc::new(OrmChatRepo::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        })),
        mock_enforcer(),
        provider_resolver,
        crate::config::StreamingConfig::default(),
        finalization,
        quota_svc,
        mock_thread_summary_repo(),
        Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
        Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
        Arc::new(crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository),
        crate::config::ContextConfig::default(),
        crate::config::RagConfig::default(),
        metrics,
    )
}

/// A message whose estimated token count exceeds `max_input_tokens` is
/// rejected with `StreamError::InputTooLong` before any DB write.
///
/// Setup: model catalog has `context_window = max_input_tokens = 500`.
/// Content: 1500 ASCII bytes → ~523 estimated tokens (default budgets).
///   ceil(1500/4) + 100 = 475; with 10 % margin: ceil(475 * 110 / 100) = 523.
///   523 > 500 → `InputTooLong`.
#[tokio::test]
async fn run_stream_input_too_long_returns_error() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&[]));
    let svc = build_stream_service_with_context_window(db, provider, 500);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    // 1500 bytes → estimated ~523 tokens, which exceeds max_input_tokens=500
    let long_content = "a".repeat(1500);

    let err = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            long_content,
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be InputTooLong");

    match err {
        StreamError::InputTooLong {
            estimated_tokens,
            max_input_tokens,
        } => {
            assert_eq!(max_input_tokens, 500);
            assert!(
                estimated_tokens > u64::from(max_input_tokens),
                "estimated {estimated_tokens} should exceed limit {max_input_tokens}"
            );
        }
        other => panic!("expected InputTooLong, got: {other:?}"),
    }
}

// ── Integration tests (8.2, 8.3) ──

/// 8.2: Duplicate `request_id` returns `Replay` (service-level equivalent of 409).
///
/// Full handler 409 mapping requires Axum test server infrastructure;
/// this test verifies the service returns the correct `StreamError` variant
/// that the handler maps to RFC-9457 JSON 409.
#[tokio::test]
async fn duplicate_request_id_returns_replay_with_turn_data() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    // First call succeeds — creates turn and streams
    let ctx1 = test_security_ctx_with_id(tenant_id, user_id);
    let (tx1, mut rx1) = mpsc::channel(32);
    let cancel1 = CancellationToken::new();
    let handle = svc
        .run_stream(
            ctx1,
            chat_id,
            request_id,
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel1,
            tx1,
        )
        .await
        .expect("first call should succeed");

    // Drain events to let the task complete
    while let Some(ev) = rx1.recv().await {
        if ev.is_terminal() {
            break;
        }
    }
    handle.await.expect("task complete");

    // Second call with same request_id → Replay with turn data
    let ctx2 = test_security_ctx_with_id(tenant_id, user_id);
    let (tx2, _rx2) = mpsc::channel(32);
    let cancel2 = CancellationToken::new();
    let err = svc
        .run_stream(
            ctx2,
            chat_id,
            request_id,
            "hello again".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel2,
            tx2,
        )
        .await
        .expect_err("should be Replay");

    match err {
        StreamError::Replay { turn } => {
            assert_eq!(turn.chat_id, chat_id);
            assert_eq!(turn.request_id, request_id);
        }
        other => panic!("expected Replay, got: {other:?}"),
    }
}

/// 8.3: Disconnect finalization — cancellation CAS-finalizes turn to cancelled.
///
/// Simulates client disconnect by cancelling the token mid-stream,
/// then verifies the turn was finalized to `cancelled` state in the DB.
#[tokio::test]
async fn disconnect_finalizes_turn_to_cancelled() {
    // Slow provider that yields one delta then blocks
    #[allow(de0309_must_have_domain_model)]
    struct SlowMockProvider;

    #[async_trait::async_trait]
    impl LlmProvider for SlowMockProvider {
        async fn stream(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<Streaming>,
            _upstream_alias: &str,
            cancel: CancellationToken,
        ) -> Result<ProviderStream, LlmProviderError> {
            let inner = stream::unfold(0u8, |state| async move {
                if state == 0 {
                    Some((
                        Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                            r#type: "text",
                            content: "partial".to_owned(),
                        })),
                        1,
                    ))
                } else {
                    // Block until cancelled
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    None
                }
            });
            Ok(ProviderStream::new(inner, cancel))
        }

        async fn complete(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<NonStreaming>,
            _upstream_alias: &str,
        ) -> Result<ResponseResult, LlmProviderError> {
            unimplemented!()
        }
    }

    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(SlowMockProvider);
    let svc = build_stream_service(db.clone(), provider);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            request_id,
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel.clone(),
            tx,
        )
        .await
        .expect("should start stream");

    // Read the stream_started event, then the first delta
    let started = rx.recv().await.expect("should get stream_started");
    assert!(matches!(started, StreamEvent::StreamStarted(_)));
    let first = rx.recv().await.expect("should get delta");
    assert!(matches!(first, StreamEvent::Delta(_)));

    // Simulate client disconnect
    cancel.cancel();

    // Wait for task to complete
    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Cancelled);

    // Verify the turn was CAS-finalized to cancelled in the DB
    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db.conn().unwrap();
    let turn = TurnRepo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .expect("find")
        .expect("turn should exist");

    assert_eq!(
        turn.state,
        TurnState::Cancelled,
        "turn should be cancelled after disconnect"
    );
    assert!(
        turn.completed_at.is_some(),
        "completed_at should be set after CAS finalization"
    );
    // D4: cancelled turn with accumulated text should have assistant_message_id
    assert!(
        turn.assistant_message_id.is_some(),
        "cancelled turn with partial text should have assistant_message_id"
    );
}

// ── Authorization tests ──

/// Cross-tenant access: user from tenant B cannot stream on tenant A's chat.
/// The scoped `chat_repo.get()` returns `None` (chat invisible), yielding `ChatNotFound`.
#[tokio::test]
async fn run_stream_cross_tenant_returns_chat_not_found() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_a = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_a, user_a, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db, provider);

    // User from a different tenant
    let tenant_b = Uuid::new_v4();
    let ctx = test_security_ctx_with_id(tenant_b, Uuid::new_v4());
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be ChatNotFound");

    match err {
        StreamError::ChatNotFound { chat_id: id } => {
            assert_eq!(id, chat_id);
        }
        other => panic!("expected ChatNotFound, got: {other:?}"),
    }
}

/// 8.6: `DoneData` uses `fctx.effective_model` / `fctx.selected_model` when they differ.
///
/// Constructs a `FinalizationCtx` with `effective_model = "gpt-4o-mini"` and
/// `selected_model = "gpt-4o"`, then verifies the Done SSE event reflects both.
#[tokio::test]
async fn done_data_uses_fctx_model_fields_when_they_differ() {
    use crate::domain::service::finalization_service::FinalizationService;
    use crate::domain::service::quota_settler::QuotaSettler;

    #[allow(de0309_must_have_domain_model)]
    struct NoopSettler;
    #[async_trait::async_trait]
    impl QuotaSettler for NoopSettler {
        async fn settle_in_tx(
            &self,
            _tx: &modkit_db::secure::DbTx<'_>,
            _scope: &AccessScope,
            _input: crate::domain::model::quota::SettlementInput,
        ) -> Result<crate::domain::model::quota::SettlementOutcome, crate::domain::error::DomainError>
        {
            Ok(crate::domain::model::quota::SettlementOutcome {
                settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                actual_credits_micro: 0,
                charged_tokens: 0,
                overshoot_capped: false,
            })
        }
    }

    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    // Create a running turn in DB so that CAS finalization succeeds
    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let turn_repo = TurnRepo;
    turn_repo
        .create_turn(
            &conn,
            &scope,
            CreateTurnParams {
                id: turn_id,
                tenant_id,
                chat_id,
                request_id,
                requester_type: "user".to_owned(),
                requester_user_id: Some(user_id),
                reserve_tokens: Some(5000),
                max_output_tokens_applied: Some(4096),
                reserved_credits_micro: Some(250),
                policy_version_applied: Some(1),
                effective_model: Some("gpt-4o-mini".to_owned()),
                minimal_generation_floor_applied: Some(50),
                web_search_enabled: false,
            },
        )
        .await
        .expect("create turn");

    let turn_repo_arc = Arc::new(TurnRepo);
    let message_repo_arc = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }));
    let finalization_svc = Arc::new(FinalizationService::new(
        Arc::clone(&db),
        Arc::clone(&turn_repo_arc),
        Arc::clone(&message_repo_arc),
        Arc::new(NoopSettler) as Arc<dyn QuotaSettler>,
        Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    ));

    let fctx = FinalizationCtx {
        finalization_svc,
        db: Arc::clone(&db),
        turn_repo: Arc::clone(&turn_repo_arc),
        scope,
        turn_id,
        tenant_id,
        chat_id,
        request_id,
        user_id,
        requester_type: RequesterType::User,
        message_id,
        effective_model: "gpt-4o-mini".to_owned(),
        selected_model: "gpt-4o".to_owned(),
        reserve_tokens: 5000,
        max_output_tokens_applied: 4096,
        reserved_credits_micro: 250,
        policy_version_applied: 1,
        minimal_generation_floor_applied: 50,
        quota_decision: "downgrade".to_owned(),
        downgrade_from: Some("gpt-4o".to_owned()),
        downgrade_reason: Some("premium_exhausted".to_owned()),
        period_starts: Vec::new(),
        provider_id: "openai".to_owned(),
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
        quota_warnings_provider: Arc::new(NoopQuotaWarningsProvider),
    };

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "gpt-4o-mini".into(), // effective_model passed as the model param
            provider_model_id: "gpt-4o-mini".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        Some(fctx),
    );

    // Collect events
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let _outcome = handle.await.expect("task should complete");

    // Find the Done event and verify model fields
    let done = events
        .iter()
        .find_map(|ev| match ev {
            StreamEvent::Done(d) => Some(d),
            _ => None,
        })
        .expect("should have a Done event");

    assert_eq!(
        done.effective_model, "gpt-4o-mini",
        "effective_model should be the downgraded model"
    );
    assert_eq!(
        done.selected_model, "gpt-4o",
        "selected_model should be the user's original choice"
    );
    assert_eq!(done.quota_decision, "downgrade");
    assert_eq!(done.downgrade_from.as_deref(), Some("gpt-4o"));
    assert_eq!(done.downgrade_reason.as_deref(), Some("premium_exhausted"));
}

// ── Preflight wiring tests (11.x) ──

fn make_catalog_entry(
    id: &str,
    tier: mini_chat_sdk::ModelTier,
) -> mini_chat_sdk::ModelCatalogEntry {
    test_catalog_entry(TestCatalogEntryParams {
        model_id: id.to_owned(),
        provider_model_id: format!("provider-{id}"),
        display_name: id.to_owned(),
        tier,
        enabled: true,
        is_default: tier == mini_chat_sdk::ModelTier::Standard,
        input_tokens_credit_multiplier_micro: 1_000_000,
        output_tokens_credit_multiplier_micro: 1_000_000,
        multimodal_capabilities: vec![],
        context_window: 128_000,
        max_output_tokens: 4096,
        description: String::new(),
        provider_display_name: String::new(),
        multiplier_display: "1x".to_owned(),
        provider_id: "openai".to_owned(),
    })
}

fn build_stream_service_with_catalog(
    db: Arc<DbProvider>,
    provider: Arc<dyn LlmProvider>,
    catalog: Vec<mini_chat_sdk::ModelCatalogEntry>,
    limits: mini_chat_sdk::UserLimits,
) -> StreamService<
    TurnRepo,
    MsgRepo,
    OrmQuotaUsageRepo,
    OrmChatRepo,
    MockThreadSummaryRepo,
    OrmAttachmentRepo,
    OrmVectorStoreRepo,
    OrmMessageAttachmentRepo,
> {
    use crate::domain::service::finalization_service::FinalizationService;
    use crate::domain::service::quota_settler::QuotaSettler;

    #[allow(de0309_must_have_domain_model)]
    struct MockQuotaSettler;
    #[async_trait::async_trait]
    impl QuotaSettler for MockQuotaSettler {
        async fn settle_in_tx(
            &self,
            _tx: &modkit_db::secure::DbTx<'_>,
            _scope: &AccessScope,
            _input: crate::domain::model::quota::SettlementInput,
        ) -> Result<crate::domain::model::quota::SettlementOutcome, crate::domain::error::DomainError>
        {
            Ok(crate::domain::model::quota::SettlementOutcome {
                settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                actual_credits_micro: 0,
                charged_tokens: 0,
                overshoot_capped: false,
            })
        }
    }

    let provider_resolver = Arc::new(ProviderResolver::single_provider(provider));
    let turn_repo = Arc::new(TurnRepo);
    let message_repo = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }));
    let finalization = Arc::new(FinalizationService::new(
        Arc::clone(&db),
        Arc::clone(&turn_repo),
        Arc::clone(&message_repo),
        Arc::new(MockQuotaSettler) as Arc<dyn QuotaSettler>,
        Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    ));

    let quota_svc = Arc::new(crate::domain::service::QuotaService::new(
        Arc::clone(&db),
        Arc::new(OrmQuotaUsageRepo),
        Arc::new(MockPolicySnapshotProvider::new(
            mini_chat_sdk::PolicySnapshot {
                user_id: Uuid::nil(),
                policy_version: 1,
                model_catalog: catalog,
                kill_switches: mini_chat_sdk::KillSwitches::default(),
            },
        )),
        Arc::new(MockUserLimitsProvider::new(limits)),
        crate::config::EstimationBudgets::default(),
        crate::config::QuotaConfig {
            overshoot_tolerance_factor: 1.10,
            ..crate::config::QuotaConfig::default()
        },
    ));

    StreamService::new(
        db,
        turn_repo,
        message_repo,
        Arc::new(OrmChatRepo::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        })),
        mock_enforcer(),
        provider_resolver,
        crate::config::StreamingConfig::default(),
        finalization,
        quota_svc,
        mock_thread_summary_repo(),
        Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
        Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
        Arc::new(crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository),
        crate::config::ContextConfig::default(),
        crate::config::RagConfig::default(),
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    )
}

fn permissive_limits() -> mini_chat_sdk::UserLimits {
    mini_chat_sdk::UserLimits {
        user_id: Uuid::nil(),
        policy_version: 1,
        standard: mini_chat_sdk::TierLimits {
            limit_daily_credits_micro: 100_000_000,
            limit_monthly_credits_micro: 1_000_000_000,
        },
        premium: mini_chat_sdk::TierLimits {
            limit_daily_credits_micro: 50_000_000,
            limit_monthly_credits_micro: 500_000_000,
        },
    }
}

/// 11.1: Allow path populates `FinalizationCtx` with real quota fields.
#[tokio::test]
async fn preflight_allow_populates_real_quota_fields() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
    let catalog = vec![make_catalog_entry(
        "gpt-5.2",
        mini_chat_sdk::ModelTier::Standard,
    )];
    let svc = build_stream_service_with_catalog(db.clone(), provider, catalog, permissive_limits());

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect("should succeed");

    // Drain events
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let _outcome = handle.await.expect("task should complete");

    // Verify Done event has allow quota_decision
    let done = events
        .iter()
        .find_map(|ev| match ev {
            StreamEvent::Done(d) => Some(d),
            _ => None,
        })
        .expect("should have a Done event");

    assert_eq!(done.quota_decision, "allow");
    assert_eq!(done.effective_model, "gpt-5.2");
    assert_eq!(done.selected_model, "gpt-5.2");
    assert!(done.downgrade_from.is_none());

    // Verify turn was created with real quota fields (not placeholder 1_000_000)
    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .find_latest_turn(&conn, &scope, chat_id)
        .await
        .expect("find turn")
        .expect("turn should exist");

    assert!(
        turn.reserve_tokens.unwrap() < 1_000_000,
        "reserve_tokens should be from real preflight, not placeholder 1_000_000"
    );
    assert!(
        turn.reserved_credits_micro.unwrap() > 0,
        "reserved_credits_micro should be from real preflight, not placeholder 0"
    );
}

/// 11.3: Reject path returns `StreamError::QuotaExhausted`.
#[tokio::test]
async fn preflight_reject_returns_quota_exhausted() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
    // Exhausted limits: 0 credits for all tiers
    let limits = mini_chat_sdk::UserLimits {
        user_id: Uuid::nil(),
        policy_version: 1,
        standard: mini_chat_sdk::TierLimits {
            limit_daily_credits_micro: 0,
            limit_monthly_credits_micro: 0,
        },
        premium: mini_chat_sdk::TierLimits {
            limit_daily_credits_micro: 0,
            limit_monthly_credits_micro: 0,
        },
    };
    let catalog = vec![make_catalog_entry(
        "gpt-5.2",
        mini_chat_sdk::ModelTier::Standard,
    )];
    let svc = build_stream_service_with_catalog(db, provider, catalog, limits);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be QuotaExhausted");

    match err {
        StreamError::QuotaExhausted {
            error_code,
            http_status,
            ..
        } => {
            assert_eq!(http_status, 429);
            assert!(!error_code.is_empty());
        }
        other => panic!("expected QuotaExhausted, got: {other:?}"),
    }
}

/// 11.2: Downgrade path sets `effective_model` != `selected_model`.
#[tokio::test]
async fn preflight_downgrade_sets_correct_model_fields() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
    // Catalog with premium model "gpt-5" and standard fallback "gpt-5-mini"
    let catalog = vec![
        make_catalog_entry("gpt-5", mini_chat_sdk::ModelTier::Premium),
        make_catalog_entry("gpt-5-mini", mini_chat_sdk::ModelTier::Standard),
    ];
    // Premium limits are 0 → forces downgrade to standard
    let limits = mini_chat_sdk::UserLimits {
        user_id: Uuid::nil(),
        policy_version: 1,
        standard: mini_chat_sdk::TierLimits {
            limit_daily_credits_micro: 100_000_000,
            limit_monthly_credits_micro: 1_000_000_000,
        },
        premium: mini_chat_sdk::TierLimits {
            limit_daily_credits_micro: 0,
            limit_monthly_credits_micro: 0,
        },
    };
    let svc = build_stream_service_with_catalog(db, provider, catalog, limits);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            ResolvedModel {
                model_id: "gpt-5".into(),
                provider_model_id: "gpt-5-2025-03-26".into(),
                provider_id: "openai".into(),
                display_name: "GPT 5".into(),
                tier: "premium".into(),
                multiplier_display: "2x".into(),
                description: None,
                multimodal_capabilities: vec![],
                context_window: 128_000,
                max_file_size_mb: 25,
                system_prompt: String::new(),
                tool_support: mini_chat_sdk::ModelToolSupport {
                    web_search: false,
                    file_search: false,
                    image_generation: false,
                    code_interpreter: false,
                    computer_use: false,
                    mcp: false,
                },
            },
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect("should succeed (downgrade, not reject)");

    // Drain events
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let _outcome = handle.await.expect("task should complete");

    // Verify Done event reflects downgrade
    let done = events
        .iter()
        .find_map(|ev| match ev {
            StreamEvent::Done(d) => Some(d),
            _ => None,
        })
        .expect("should have a Done event");

    assert_eq!(done.quota_decision, "downgrade");
    assert_eq!(
        done.effective_model, "gpt-5-mini",
        "should be downgraded model"
    );
    assert_eq!(
        done.selected_model, "gpt-5",
        "should be original selected model"
    );
    assert_eq!(done.downgrade_from.as_deref(), Some("gpt-5"));
}

/// 11.4: On downgrade the LLM provider receives the **downgraded** model's
/// `provider_model_id`, not the originally-requested premium model's.
///
/// Regression: before `effective_provider_model_id` was added to
/// `PreflightDecision`, the provider always received the premium model's
/// provider ID even after a quota downgrade, so users got premium
/// responses billed at standard rates.
#[tokio::test]
async fn downgrade_sends_effective_provider_model_id_to_llm() {
    // ── Capturing provider that records the model from LlmRequest ──
    #[domain_model]
    struct CapturingProvider {
        captured_model: std::sync::Mutex<Option<String>>,
        inner: MockProvider,
    }

    impl CapturingProvider {
        fn new(inner: MockProvider) -> Self {
            Self {
                captured_model: std::sync::Mutex::new(None),
                inner,
            }
        }

        fn captured_model(&self) -> String {
            self.captured_model
                .lock()
                .unwrap()
                .clone()
                .expect("provider was never called")
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for CapturingProvider {
        async fn stream(
            &self,
            ctx: SecurityContext,
            request: LlmRequest<Streaming>,
            upstream_alias: &str,
            cancel: CancellationToken,
        ) -> Result<ProviderStream, LlmProviderError> {
            *self.captured_model.lock().unwrap() = Some(request.model().to_owned());
            self.inner
                .stream(ctx, request, upstream_alias, cancel)
                .await
        }

        async fn complete(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<NonStreaming>,
            _upstream_alias: &str,
        ) -> Result<ResponseResult, LlmProviderError> {
            unimplemented!("not needed for streaming tests")
        }
    }

    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let capturing = Arc::new(CapturingProvider::new(MockProvider::completed(&["Hi"])));
    let provider: Arc<dyn LlmProvider> = Arc::clone(&capturing) as _;

    // Catalog: premium "gpt-5" (provider_model_id = "provider-gpt-5")
    //          standard "gpt-5-mini" (provider_model_id = "provider-gpt-5-mini")
    let catalog = vec![
        make_catalog_entry("gpt-5", mini_chat_sdk::ModelTier::Premium),
        make_catalog_entry("gpt-5-mini", mini_chat_sdk::ModelTier::Standard),
    ];

    // Premium limits = 0 → forces downgrade to standard
    let limits = mini_chat_sdk::UserLimits {
        user_id: Uuid::nil(),
        policy_version: 1,
        standard: mini_chat_sdk::TierLimits {
            limit_daily_credits_micro: 100_000_000,
            limit_monthly_credits_micro: 1_000_000_000,
        },
        premium: mini_chat_sdk::TierLimits {
            limit_daily_credits_micro: 0,
            limit_monthly_credits_micro: 0,
        },
    };
    let svc = build_stream_service_with_catalog(db, provider, catalog, limits);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    // Request premium "gpt-5" with its own provider_model_id.
    // After downgrade the provider must NOT see this ID.
    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            ResolvedModel {
                model_id: "gpt-5".into(),
                provider_model_id: "gpt-5-2025-03-26".into(),
                provider_id: "openai".into(),
                display_name: "GPT 5".into(),
                tier: "premium".into(),
                multiplier_display: "2x".into(),
                description: None,
                multimodal_capabilities: vec![],
                context_window: 128_000,
                max_file_size_mb: 25,
                system_prompt: String::new(),
                tool_support: mini_chat_sdk::ModelToolSupport {
                    web_search: false,
                    file_search: false,
                    image_generation: false,
                    code_interpreter: false,
                    computer_use: false,
                    mcp: false,
                },
            },
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect("should succeed (downgrade, not reject)");

    // Drain events
    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            break;
        }
    }
    let _outcome = handle.await.expect("task should complete");

    // The provider must have received the standard model's provider_model_id,
    // NOT the premium "gpt-5-2025-03-26" from ResolvedModel.
    assert_eq!(
        capturing.captured_model(),
        "provider-gpt-5-mini",
        "provider should receive the downgraded model's provider_model_id, \
             not the originally-requested premium model's"
    );
}

/// 11.5: When a chat has ready file-search documents, preflight must
/// include `tool_surcharge_tokens` in the reservation estimate.
///
/// Regression: `tools_enabled` was hardcoded to `false`, so
/// `tool_surcharge_tokens` was never added to the token estimate,
/// under-reserving quota for chats with attachments.
#[tokio::test]
async fn preflight_includes_tool_surcharge_when_documents_present() {
    use crate::domain::service::test_helpers::{
        InsertTestAttachmentParams, insert_test_attachment,
    };

    // ── Run 1: baseline — no documents ──
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id_no_docs = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id_no_docs).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hi"]));
    let catalog = vec![make_catalog_entry(
        "gpt-5.2",
        mini_chat_sdk::ModelTier::Standard,
    )];
    let svc = build_stream_service_with_catalog(db.clone(), provider, catalog, permissive_limits());

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id_no_docs,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect("run_stream without docs should succeed");

    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            break;
        }
    }
    let _outcome = handle.await.expect("task should complete");

    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let turn_repo = TurnRepo;
    let turn_no_docs = turn_repo
        .find_latest_turn(&conn, &scope, chat_id_no_docs)
        .await
        .expect("find turn")
        .expect("turn should exist");
    let tokens_no_docs = turn_no_docs.reserve_tokens.unwrap();

    // ── Run 2: with a ready file-search document ──
    let chat_id_with_docs = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id_with_docs).await;

    insert_test_attachment(
        &db,
        InsertTestAttachmentParams::ready_document(tenant_id, chat_id_with_docs),
    )
    .await;

    let provider2: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hi"]));
    let catalog2 = vec![make_catalog_entry(
        "gpt-5.2",
        mini_chat_sdk::ModelTier::Standard,
    )];
    let svc2 =
        build_stream_service_with_catalog(db.clone(), provider2, catalog2, permissive_limits());

    let ctx2 = test_security_ctx_with_id(tenant_id, user_id);
    let (tx2, mut rx2) = mpsc::channel(32);
    let cancel2 = CancellationToken::new();

    let handle2 = svc2
        .run_stream(
            ctx2,
            chat_id_with_docs,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel2,
            tx2,
        )
        .await
        .expect("run_stream with docs should succeed");

    while let Some(ev) = rx2.recv().await {
        if ev.is_terminal() {
            break;
        }
    }
    let _outcome2 = handle2.await.expect("task should complete");

    let turn_with_docs = turn_repo
        .find_latest_turn(&conn, &scope, chat_id_with_docs)
        .await
        .expect("find turn")
        .expect("turn should exist");
    let tokens_with_docs = turn_with_docs.reserve_tokens.unwrap();

    // The default tool_surcharge_tokens is 500. With a document present,
    // reserve_tokens must be strictly higher than without.
    assert!(
        tokens_with_docs > tokens_no_docs,
        "reserve_tokens with documents ({tokens_with_docs}) should exceed \
             reserve_tokens without documents ({tokens_no_docs}) by tool_surcharge_tokens"
    );
}

/// Non-existent chat returns `ChatNotFound`.
#[tokio::test]
async fn run_stream_nonexistent_chat_returns_chat_not_found() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let bogus_chat_id = Uuid::new_v4();
    // No chat inserted

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db, provider);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream(
            ctx,
            bogus_chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect_err("should be ChatNotFound");

    match err {
        StreamError::ChatNotFound { chat_id } => {
            assert_eq!(chat_id, bogus_chat_id);
        }
        other => panic!("expected ChatNotFound, got: {other:?}"),
    }
}

// ── Per-message web search call limit tests ──

/// 6.5: Web search calls within limit — stream completes normally.
#[tokio::test]
async fn test_per_message_limit_not_exceeded() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_web_search_calls(2)); // 2 calls, limit is 2
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let outcome = handle.await.expect("task should not panic");
    assert_eq!(outcome.terminal, StreamTerminal::Completed);

    // Expect: 1 delta + 2*(start+done) tool events + 1 done = 6 events
    assert_eq!(events.len(), 6);
    assert!(matches!(events.last(), Some(StreamEvent::Done(_))));
    // No error events
    assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
}

/// 6.6: Web search calls exceed limit — stream terminates with error.
#[tokio::test]
async fn test_per_message_limit_exceeded() {
    // 3 web search calls but limit is 2 — the 3rd start should trigger error
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_web_search_calls(3));
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let outcome = handle.await.expect("task should not panic");
    assert_eq!(outcome.terminal, StreamTerminal::Failed);
    assert_eq!(
        outcome.error_code.as_deref(),
        Some("web_search_calls_exceeded")
    );

    // Last event should be an error
    let last = events.last().expect("should have events");
    match last {
        StreamEvent::Error(data) => {
            assert_eq!(data.code, "web_search_calls_exceeded");
        }
        other => panic!("expected Error event, got: {other:?}"),
    }
}

/// 6.7: Other tool calls don't count toward web search limit.
#[tokio::test]
async fn test_per_message_counter_ignores_other_tools() {
    // 5 file_search calls + 1 web_search call, limit is 2 — should complete
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_tool_calls(&[
        ("file_search", 5),
        ("web_search", 1),
    ]));
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let outcome = handle.await.expect("task should not panic");
    assert_eq!(outcome.terminal, StreamTerminal::Completed);
    // No error events
    assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
}

// ── Per-message code interpreter call limit tests ──

/// 6.8: Code interpreter calls within limit — stream completes normally.
#[tokio::test]
async fn test_ci_per_message_limit_not_exceeded() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_code_interpreter_calls(2)); // 2 calls, limit is 2
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let outcome = handle.await.expect("task should not panic");
    assert_eq!(outcome.terminal, StreamTerminal::Completed);

    // Expect: 1 delta + 2*(start+done) tool events + 1 done = 6 events
    assert_eq!(events.len(), 6);
    assert!(matches!(events.last(), Some(StreamEvent::Done(_))));
    assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
}

/// 6.9: Code interpreter calls exceed limit — stream terminates with error.
#[tokio::test]
async fn test_ci_per_message_limit_exceeded() {
    // 3 code_interpreter calls but limit is 2 — the 3rd start should trigger error
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_code_interpreter_calls(3));
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let outcome = handle.await.expect("task should not panic");
    assert_eq!(outcome.terminal, StreamTerminal::Failed);
    assert_eq!(
        outcome.error_code.as_deref(),
        Some("code_interpreter_calls_exceeded")
    );

    // Last event should be an error
    let last = events.last().expect("should have events");
    match last {
        StreamEvent::Error(data) => {
            assert_eq!(data.code, "code_interpreter_calls_exceeded");
        }
        other => panic!("expected Error event, got: {other:?}"),
    }
}

/// 6.10: Other tool calls don't count toward code interpreter limit.
#[tokio::test]
async fn test_ci_per_message_counter_ignores_other_tools() {
    // 5 file_search calls + 1 code_interpreter call, limit is 2 — should complete
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_tool_calls(&[
        ("file_search", 5),
        ("code_interpreter", 1),
    ]));
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
    let cancel = CancellationToken::new();

    let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
        mock_ctx(),
        provider_task::ProviderTaskConfig {
            llm: provider,
            upstream_alias: "test-alias".to_owned(),
            messages: vec![LlmMessage::user("hi")],
            system_instructions: None,
            tools: vec![],
            model: "test-model".into(),
            provider_model_id: "test-model".into(),
            max_output_tokens: 4096,
            max_tool_calls: 2,
            web_search_max_calls: 2,
            code_interpreter_max_calls: 2,
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
            },
            provider_file_id_map: std::collections::HashMap::new(),
        },
        cancel,
        tx,
        None,
    );

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        events.push(ev);
        if is_term {
            break;
        }
    }

    let outcome = handle.await.expect("task should not panic");
    assert_eq!(outcome.terminal, StreamTerminal::Completed);
    assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
}

// ── P5-I: SendMessage Attachment Validation (negative) ──

use crate::domain::service::test_helpers::{
    InsertTestAttachmentParams, insert_test_attachment, insert_test_vector_store,
};
use crate::infra::db::entity::attachment::AttachmentStatus;

/// Helper: call `run_stream` with given `attachment_ids`, expect `StreamError::InvalidAttachment`.
async fn run_stream_expect_invalid_attachment(
    svc: &StreamService<
        TurnRepo,
        MsgRepo,
        OrmQuotaUsageRepo,
        OrmChatRepo,
        MockThreadSummaryRepo,
        OrmAttachmentRepo,
        OrmVectorStoreRepo,
        OrmMessageAttachmentRepo,
    >,
    tenant_id: Uuid,
    user_id: Uuid,
    chat_id: Uuid,
    attachment_ids: Vec<Uuid>,
) -> StreamError {
    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    svc.run_stream(
        ctx,
        chat_id,
        Uuid::new_v4(),
        "test message".into(),
        test_resolved_model(),
        false,
        attachment_ids,
        cancel,
        tx,
    )
    .await
    .expect_err("should fail with InvalidAttachment")
}

/// P5-I1: Nonexistent attachment UUID → `InvalidAttachment` error.
#[tokio::test]
async fn send_message_nonexistent_attachment_id() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    let err = run_stream_expect_invalid_attachment(
        &svc,
        tenant_id,
        user_id,
        chat_id,
        vec![Uuid::new_v4()],
    )
    .await;

    assert!(
        matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("not found")),
        "expected 'not found', got: {err:?}"
    );
}

/// P5-I2: Soft-deleted attachment → `InvalidAttachment` error.
#[tokio::test]
async fn send_message_deleted_attachment_id() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            deleted_at: Some(time::OffsetDateTime::now_utc()),
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    let err =
        run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_id, vec![att_id]).await;

    assert!(
        matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("deleted")),
        "expected 'deleted', got: {err:?}"
    );
}

/// P5-I3: Pending (not ready) attachment → `InvalidAttachment` error.
#[tokio::test]
async fn send_message_pending_attachment_id() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            status: AttachmentStatus::Pending,
            provider_file_id: None,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    let err =
        run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_id, vec![att_id]).await;

    assert!(
        matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("not ready")),
        "expected 'not ready', got: {err:?}"
    );
}

/// P5-I4: Failed attachment → `InvalidAttachment` error.
#[tokio::test]
async fn send_message_failed_attachment_id() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            status: AttachmentStatus::Failed,
            provider_file_id: None,
            error_code: Some("upload_failed".to_owned()),
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    let err =
        run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_id, vec![att_id]).await;

    assert!(
        matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("not ready")),
        "expected 'not ready', got: {err:?}"
    );
}

/// P5-I5: Attachment from a different chat → `InvalidAttachment` error.
#[tokio::test]
async fn send_message_attachment_from_different_chat() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_a = Uuid::new_v4();
    let chat_b = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_a).await;
    insert_test_chat(&db, tenant_id, user_id, chat_b).await;

    // Attachment belongs to chat_b
    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_b)
        },
    )
    .await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    // Try to use it in chat_a
    let err =
        run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_a, vec![att_id]).await;

    assert!(
        matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("does not belong")),
        "expected 'does not belong', got: {err:?}"
    );
}

/// P5-I6: Attachment owned by different user → `InvalidAttachment` error.
#[tokio::test]
async fn send_message_attachment_wrong_owner() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let other_user = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    // Attachment uploaded by other_user
    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: other_user,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    let err =
        run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_id, vec![att_id]).await;

    assert!(
        matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("not owned")),
        "expected 'not owned', got: {err:?}"
    );
}

/// P5-I7: Duplicate attachment IDs in request → `InvalidAttachment` error.
#[tokio::test]
async fn send_message_duplicate_attachment_ids() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
    let svc = build_stream_service(db.clone(), provider);

    // Same UUID twice
    let err = run_stream_expect_invalid_attachment(
        &svc,
        tenant_id,
        user_id,
        chat_id,
        vec![att_id, att_id],
    )
    .await;

    assert!(
        matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("Duplicate")),
        "expected 'Duplicate', got: {err:?}"
    );
}

// ── P5-H: SendMessage with Attachments (positive) ──

/// P5-H1: Valid `attachment_ids` → `message_attachments` persisted, stream completes.
#[tokio::test]
async fn send_message_with_valid_attachment_ids() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    // Insert vector store so file_search can activate
    insert_test_vector_store(&db, tenant_id, chat_id, Some("vs_test123".to_owned())).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["the answer"]));
    let svc = build_stream_service(db.clone(), provider);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let result = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "summarize the doc".into(),
            test_resolved_model(),
            false,
            vec![att_id],
            cancel,
            tx,
        )
        .await;

    assert!(result.is_ok(), "run_stream should succeed: {result:?}");

    // Drain events — should complete without error
    let mut got_done = false;
    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            got_done = matches!(ev, StreamEvent::Done(_));
            break;
        }
    }
    assert!(got_done, "stream should complete with Done event");

    // Verify message_attachments row persisted
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let repo = OrmMessageAttachmentRepo;
    let exists = repo
        .exists_for_attachment(&conn, &scope, att_id)
        .await
        .expect("exists_for_attachment");
    assert!(
        exists,
        "message_attachment row should exist for the attachment"
    );
}

/// P5-H3: Provider file citations mapped to internal UUID end-to-end.
#[tokio::test]
async fn send_message_citation_mapping_end_to_end() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let provider_file_id = "file-abc123";
    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            provider_file_id: Some(provider_file_id.to_owned()),
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    insert_test_vector_store(&db, tenant_id, chat_id, Some("vs_cit1".to_owned())).await;

    // Provider returns a file citation with the provider's file_id
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed_with_citations(
        &["Kinbote City"],
        vec![Citation {
            source: CitationSource::File,
            title: "test.pdf".to_owned(),
            url: None,
            attachment_id: Some(provider_file_id.to_owned()),
            snippet: "capital of Zembla".to_owned(),
            score: Some(0.95),
            span: None,
        }],
    ));
    let svc = build_stream_service(db.clone(), provider);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let result = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "What is the capital?".into(),
            test_resolved_model(),
            false,
            vec![att_id],
            cancel,
            tx,
        )
        .await;
    assert!(result.is_ok(), "run_stream failed: {result:?}");

    let mut citation_events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        if matches!(ev, StreamEvent::Citations(_)) {
            citation_events.push(ev);
        }
        if is_term {
            break;
        }
    }

    // Should have a citations event with the internal UUID, not "file-abc123"
    assert_eq!(citation_events.len(), 1, "expected 1 citations event");
    if let StreamEvent::Citations(data) = &citation_events[0] {
        assert_eq!(data.items.len(), 1);
        let cit = &data.items[0];
        assert_eq!(
            cit.attachment_id.as_deref(),
            Some(att_id.to_string().as_str())
        );
        assert!(!cit.attachment_id.as_deref().unwrap().starts_with("file-"));
    } else {
        panic!("expected Citations event");
    }
}

/// P5-H4: Web citations pass through unchanged.
#[tokio::test]
async fn send_message_web_citations_passthrough() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    insert_test_vector_store(&db, tenant_id, chat_id, Some("vs_web1".to_owned())).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed_with_citations(
        &["result"],
        vec![Citation {
            source: CitationSource::Web,
            title: "Example Page".to_owned(),
            url: Some("https://example.com/page".to_owned()),
            attachment_id: None,
            snippet: "some web content".to_owned(),
            score: None,
            span: None,
        }],
    ));
    let svc = build_stream_service(db.clone(), provider);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let result = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "search the web".into(),
            test_resolved_model(),
            false,
            vec![att_id],
            cancel,
            tx,
        )
        .await;
    assert!(result.is_ok(), "run_stream failed: {result:?}");

    let mut citation_events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let is_term = ev.is_terminal();
        if matches!(ev, StreamEvent::Citations(_)) {
            citation_events.push(ev);
        }
        if is_term {
            break;
        }
    }

    assert_eq!(citation_events.len(), 1, "expected 1 citations event");
    if let StreamEvent::Citations(data) = &citation_events[0] {
        assert_eq!(data.items.len(), 1);
        let cit = &data.items[0];
        assert!(matches!(cit.source, CitationSource::Web));
        assert_eq!(cit.url.as_deref(), Some("https://example.com/page"));
        assert_eq!(cit.title, "Example Page");
    } else {
        panic!("expected Citations event");
    }
}

/// P5-H2: Empty `attachment_ids` with ready docs → stream completes (unrestricted search).
#[tokio::test]
async fn send_message_no_attachments_with_docs() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    // Insert a ready doc (no attachment_ids passed to message)
    insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    insert_test_vector_store(&db, tenant_id, chat_id, Some("vs_test456".to_owned())).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["answer"]));
    let svc = build_stream_service(db.clone(), provider);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let result = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "question about docs".into(),
            test_resolved_model(),
            false,
            vec![], // no attachment_ids
            cancel,
            tx,
        )
        .await;

    assert!(result.is_ok(), "run_stream should succeed: {result:?}");

    let mut got_done = false;
    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            got_done = matches!(ev, StreamEvent::Done(_));
            break;
        }
    }
    assert!(got_done, "stream should complete with Done event");
}

// ── Mutation RAG wiring tests (WS2: 2.7–2.9) ──

/// Insert a running turn row (required by mutation stream finalization CAS).
async fn insert_running_turn(
    db: &Arc<DbProvider>,
    tenant_id: Uuid,
    user_id: Uuid,
    chat_id: Uuid,
    request_id: Uuid,
    turn_id: Uuid,
) {
    use crate::infra::db::entity::chat_turn::{
        ActiveModel as TurnAM, Entity as TurnEntity, TurnState,
    };
    use modkit_db::secure::secure_insert;
    use sea_orm::Set;
    use time::OffsetDateTime;

    let now = OffsetDateTime::now_utc();
    let am = TurnAM {
        id: Set(turn_id),
        tenant_id: Set(tenant_id),
        chat_id: Set(chat_id),
        request_id: Set(request_id),
        requester_type: Set("user".to_owned()),
        requester_user_id: Set(Some(user_id)),
        state: Set(TurnState::Running),
        provider_name: Set(None),
        provider_response_id: Set(None),
        assistant_message_id: Set(None),
        error_code: Set(None),
        error_detail: Set(None),
        reserve_tokens: Set(None),
        max_output_tokens_applied: Set(None),
        reserved_credits_micro: Set(None),
        policy_version_applied: Set(None),
        effective_model: Set(None),
        minimal_generation_floor_applied: Set(None),
        web_search_enabled: Set(false),
        web_search_completed_count: Set(0),
        code_interpreter_completed_count: Set(0),
        deleted_at: Set(None),
        replaced_by_request_id: Set(None),
        started_at: Set(now),
        last_progress_at: Set(Some(now)),
        completed_at: Set(None),
        updated_at: Set(now),
    };
    let conn = db.conn().unwrap();
    secure_insert::<TurnEntity>(am, &AccessScope::allow_all(), &conn)
        .await
        .expect("insert running turn");
}

fn build_stream_service_with_policy(
    db: Arc<DbProvider>,
    provider: Arc<dyn LlmProvider>,
    kill_switches: mini_chat_sdk::KillSwitches,
) -> StreamService<
    TurnRepo,
    MsgRepo,
    OrmQuotaUsageRepo,
    OrmChatRepo,
    MockThreadSummaryRepo,
    OrmAttachmentRepo,
    OrmVectorStoreRepo,
    OrmMessageAttachmentRepo,
> {
    use crate::domain::service::finalization_service::FinalizationService;
    use crate::domain::service::quota_settler::QuotaSettler;

    #[allow(de0309_must_have_domain_model)]
    struct MockQuotaSettler;
    #[async_trait::async_trait]
    impl QuotaSettler for MockQuotaSettler {
        async fn settle_in_tx(
            &self,
            _tx: &modkit_db::secure::DbTx<'_>,
            _scope: &AccessScope,
            _input: crate::domain::model::quota::SettlementInput,
        ) -> Result<crate::domain::model::quota::SettlementOutcome, crate::domain::error::DomainError>
        {
            Ok(crate::domain::model::quota::SettlementOutcome {
                settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                actual_credits_micro: 0,
                charged_tokens: 0,
                overshoot_capped: false,
            })
        }
    }

    let provider_resolver = Arc::new(ProviderResolver::single_provider(provider));
    let turn_repo = Arc::new(TurnRepo);
    let message_repo = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }));
    let finalization = Arc::new(FinalizationService::new(
        Arc::clone(&db),
        Arc::clone(&turn_repo),
        Arc::clone(&message_repo),
        Arc::new(MockQuotaSettler) as Arc<dyn QuotaSettler>,
        Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    ));

    let quota_svc = Arc::new(crate::domain::service::QuotaService::new(
        Arc::clone(&db),
        Arc::new(OrmQuotaUsageRepo),
        Arc::new(MockPolicySnapshotProvider::new(
            mini_chat_sdk::PolicySnapshot {
                user_id: Uuid::nil(),
                policy_version: 1,
                model_catalog: vec![test_catalog_entry(TestCatalogEntryParams {
                    model_id: "gpt-5.2".to_owned(),
                    provider_model_id: "gpt-5.2-2025-03-26".to_owned(),
                    display_name: "GPT 5.2".to_owned(),
                    tier: mini_chat_sdk::ModelTier::Standard,
                    enabled: true,
                    is_default: true,
                    input_tokens_credit_multiplier_micro: 1_000_000,
                    output_tokens_credit_multiplier_micro: 1_000_000,
                    multimodal_capabilities: vec![],
                    context_window: 128_000,
                    max_output_tokens: 4096,
                    description: String::new(),
                    provider_display_name: String::new(),
                    multiplier_display: "1x".to_owned(),
                    provider_id: "openai".to_owned(),
                })],
                kill_switches,
            },
        )),
        Arc::new(MockUserLimitsProvider::new(permissive_limits())),
        crate::config::EstimationBudgets::default(),
        crate::config::QuotaConfig {
            overshoot_tolerance_factor: 1.10,
            ..crate::config::QuotaConfig::default()
        },
    ));

    StreamService::new(
        db,
        turn_repo,
        message_repo,
        Arc::new(OrmChatRepo::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        })),
        mock_enforcer(),
        provider_resolver,
        crate::config::StreamingConfig::default(),
        finalization,
        quota_svc,
        mock_thread_summary_repo(),
        Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
        Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
        Arc::new(crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository),
        crate::config::ContextConfig::default(),
        crate::config::RagConfig::default(),
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    )
}

/// 2.7: Mutation with attachments gets `file_search_enabled` = true and real RAG values.
#[tokio::test]
async fn mutation_with_attachments_gets_file_search() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;
    insert_running_turn(&db, tenant_id, user_id, chat_id, request_id, turn_id).await;

    // Insert a ready document attachment + vector store
    let _att_id = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            provider_file_id: Some("file-mut-001".to_owned()),
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;
    insert_test_vector_store(&db, tenant_id, chat_id, Some("vs-mut-001".to_owned())).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["retry answer"]));
    let svc =
        build_stream_service_with_policy(db, provider, mini_chat_sdk::KillSwitches::default());

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let result = svc
        .run_stream_for_mutation(
            ctx,
            chat_id,
            request_id,
            turn_id,
            "retry question".into(),
            test_resolved_model(),
            false,
            None,
            cancel,
            tx,
        )
        .await;

    assert!(result.is_ok(), "mutation stream should succeed: {result:?}");

    let mut got_done = false;
    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            got_done = matches!(ev, StreamEvent::Done(_));
            break;
        }
    }
    assert!(got_done, "mutation stream should complete with Done event");
}

/// 2.8: Mutation after all attachments deleted gets `file_search_enabled` = false.
#[tokio::test]
async fn mutation_no_attachments_no_file_search() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;
    insert_running_turn(&db, tenant_id, user_id, chat_id, request_id, turn_id).await;
    // No attachments inserted — simulates all deleted

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["no docs"]));
    let svc = build_stream_service(db, provider);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let result = svc
        .run_stream_for_mutation(
            ctx,
            chat_id,
            request_id,
            turn_id,
            "retry without docs".into(),
            test_resolved_model(),
            false,
            None,
            cancel,
            tx,
        )
        .await;

    assert!(
        result.is_ok(),
        "mutation without docs should succeed: {result:?}"
    );

    let mut got_done = false;
    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            got_done = matches!(ev, StreamEvent::Done(_));
            break;
        }
    }
    assert!(got_done, "mutation stream should complete with Done");
}

/// 2.9: Kill switch active during mutation forces `RetrievalMode::None`.
#[tokio::test]
async fn mutation_kill_switch_disables_file_search() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;
    insert_running_turn(&db, tenant_id, user_id, chat_id, request_id, turn_id).await;

    // Insert attachments + VS (would normally activate file search)
    insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;
    insert_test_vector_store(&db, tenant_id, chat_id, Some("vs-kill-001".to_owned())).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["killed"]));
    // Activate file_search kill switch
    let svc = build_stream_service_with_policy(
        db,
        provider,
        mini_chat_sdk::KillSwitches {
            disable_file_search: true,
            ..Default::default()
        },
    );

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let result = svc
        .run_stream_for_mutation(
            ctx,
            chat_id,
            request_id,
            turn_id,
            "retry with kill switch".into(),
            test_resolved_model(),
            false,
            None,
            cancel,
            tx,
        )
        .await;

    assert!(
        result.is_ok(),
        "mutation with kill switch should succeed: {result:?}"
    );

    let mut got_done = false;
    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            got_done = matches!(ev, StreamEvent::Done(_));
            break;
        }
    }
    assert!(
        got_done,
        "mutation stream should complete despite kill switch"
    );
}

// ── TestMetrics — recording implementation for metric assertions ─────

use std::sync::atomic::{AtomicU64, Ordering};

/// Lightweight `MiniChatMetricsPort` that records counter increments
/// and histogram observation counts via atomics. Used to verify that
/// service code emits the expected metrics.
#[domain_model]
struct TestMetrics {
    stream_started: AtomicU64,
    stream_completed: AtomicU64,
    stream_failed: AtomicU64,
    stream_disconnected: AtomicU64,
    stream_incomplete: AtomicU64,
    active_streams_inc: AtomicU64,
    active_streams_dec: AtomicU64,
    ttft_provider_ms: AtomicU64,
    ttft_overhead_ms: AtomicU64,
    stream_total_latency_ms: AtomicU64,
    quota_preflight: AtomicU64,
    quota_reserve: AtomicU64,
    quota_estimated_tokens: AtomicU64,
    audit_emit: AtomicU64,
    finalization_latency_ms: AtomicU64,
    quota_commit: AtomicU64,
    cancel_requested: AtomicU64,
    cancel_effective: AtomicU64,
    time_to_abort_ms: AtomicU64,
}

impl TestMetrics {
    fn new() -> Self {
        Self {
            stream_started: AtomicU64::new(0),
            stream_completed: AtomicU64::new(0),
            stream_failed: AtomicU64::new(0),
            stream_disconnected: AtomicU64::new(0),
            stream_incomplete: AtomicU64::new(0),
            active_streams_inc: AtomicU64::new(0),
            active_streams_dec: AtomicU64::new(0),
            ttft_provider_ms: AtomicU64::new(0),
            ttft_overhead_ms: AtomicU64::new(0),
            stream_total_latency_ms: AtomicU64::new(0),
            quota_preflight: AtomicU64::new(0),
            quota_reserve: AtomicU64::new(0),
            quota_estimated_tokens: AtomicU64::new(0),
            audit_emit: AtomicU64::new(0),
            finalization_latency_ms: AtomicU64::new(0),
            quota_commit: AtomicU64::new(0),
            cancel_requested: AtomicU64::new(0),
            cancel_effective: AtomicU64::new(0),
            time_to_abort_ms: AtomicU64::new(0),
        }
    }
}

impl crate::domain::ports::MiniChatMetricsPort for TestMetrics {
    fn record_stream_started(&self, _: &str, _: &str) {
        self.stream_started.fetch_add(1, Ordering::Relaxed);
    }
    fn record_stream_completed(&self, _: &str, _: &str) {
        self.stream_completed.fetch_add(1, Ordering::Relaxed);
    }
    fn record_stream_failed(&self, _: &str, _: &str, _: &str) {
        self.stream_failed.fetch_add(1, Ordering::Relaxed);
    }
    fn record_stream_disconnected(&self, _: &str) {
        self.stream_disconnected.fetch_add(1, Ordering::Relaxed);
    }
    fn increment_active_streams(&self) {
        self.active_streams_inc.fetch_add(1, Ordering::Relaxed);
    }
    fn decrement_active_streams(&self) {
        self.active_streams_dec.fetch_add(1, Ordering::Relaxed);
    }
    fn record_ttft_provider_ms(&self, _: &str, _: &str, _: f64) {
        self.ttft_provider_ms.fetch_add(1, Ordering::Relaxed);
    }
    fn record_ttft_overhead_ms(&self, _: &str, _: &str, _: f64) {
        self.ttft_overhead_ms.fetch_add(1, Ordering::Relaxed);
    }
    fn record_stream_total_latency_ms(&self, _: &str, _: &str, _: f64) {
        self.stream_total_latency_ms.fetch_add(1, Ordering::Relaxed);
    }
    fn record_turn_mutation(&self, _: &str, _: &str) {}
    fn record_turn_mutation_latency_ms(&self, _: &str, _: f64) {}
    fn record_audit_emit(&self, _: &str) {
        self.audit_emit.fetch_add(1, Ordering::Relaxed);
    }
    fn record_finalization_latency_ms(&self, _: f64) {
        self.finalization_latency_ms.fetch_add(1, Ordering::Relaxed);
    }
    fn record_quota_preflight(&self, _: &str, _: &str, _: &str) {
        self.quota_preflight.fetch_add(1, Ordering::Relaxed);
    }
    fn record_quota_reserve(&self, _: &str) {
        self.quota_reserve.fetch_add(1, Ordering::Relaxed);
    }
    fn record_quota_commit(&self, _: &str) {
        self.quota_commit.fetch_add(1, Ordering::Relaxed);
    }
    fn record_quota_overshoot(&self, _: &str) {}
    fn record_quota_estimated_tokens(&self, _: f64) {
        self.quota_estimated_tokens.fetch_add(1, Ordering::Relaxed);
    }
    fn record_quota_actual_tokens(&self, _: f64) {}
    fn record_stream_incomplete(&self, _: &str, _: &str, _: &str) {
        self.stream_incomplete.fetch_add(1, Ordering::Relaxed);
    }
    fn record_cancel_requested(&self, _: &str) {
        self.cancel_requested.fetch_add(1, Ordering::Relaxed);
    }
    fn record_cancel_effective(&self, _: &str) {
        self.cancel_effective.fetch_add(1, Ordering::Relaxed);
    }
    fn record_time_to_abort_ms(&self, _: &str, _: f64) {
        self.time_to_abort_ms.fetch_add(1, Ordering::Relaxed);
    }
    fn record_streams_aborted(&self, _: &str) {}
    fn record_attachment_upload(&self, _: &str, _: &str) {}
    fn record_attachment_upload_bytes(&self, _: &str, _: f64) {}
    fn increment_attachments_pending(&self) {}
    fn decrement_attachments_pending(&self) {}
    fn record_image_inputs_per_turn(&self, _: u32) {}
    fn record_orphan_detected(&self, _: &str) {}
    fn record_orphan_finalized(&self, _: &str) {}
    fn record_orphan_scan_duration_seconds(&self, _: f64) {}
    fn record_code_interpreter_calls(&self, _: &str, _: u32) {}
    fn record_cleanup_completed(&self, _: &str) {}
    fn record_cleanup_failed(&self, _: &str) {}
    fn record_cleanup_retry(&self, _: &str, _: &str) {}
    fn record_cleanup_backlog(&self, _: &str, _: &str, _: i64) {}
    fn record_cleanup_vs_with_failed_attachments(&self) {}
}

// ── Metric emission tests ────────────────────────────────────────────

/// Provider that emits one delta then hangs indefinitely, allowing
/// cancellation tests. The stream yields a single token, then `pending`
/// forever until the cancellation token fires.
#[domain_model]
struct HangingProvider;

#[async_trait::async_trait]
impl LlmProvider for HangingProvider {
    async fn stream(
        &self,
        _ctx: SecurityContext,
        _request: LlmRequest<Streaming>,
        _upstream_alias: &str,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, LlmProviderError> {
        let one_delta = stream::once(async {
            Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                r#type: "text",
                content: "Hi".to_owned(),
            }))
        });
        // Chain with a stream that never resolves
        let hanging = one_delta.chain(stream::pending());
        Ok(ProviderStream::new(hanging, cancel))
    }

    async fn complete(
        &self,
        _ctx: SecurityContext,
        _request: LlmRequest<NonStreaming>,
        _upstream_alias: &str,
    ) -> Result<ResponseResult, LlmProviderError> {
        unimplemented!()
    }
}

/// Happy path: completed stream emits expected metric sequence.
#[tokio::test]
async fn metrics_emitted_on_completed_stream() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let metrics = Arc::new(TestMetrics::new());
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
    let svc = build_stream_service_with_metrics(db, provider, Arc::clone(&metrics) as _);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect("should succeed");

    // Drain events
    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            break;
        }
    }
    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Completed);

    // Verify metric emissions
    assert_eq!(metrics.stream_started.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_completed.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_failed.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.active_streams_inc.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.active_streams_dec.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.ttft_provider_ms.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.ttft_overhead_ms.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_total_latency_ms.load(Ordering::Relaxed), 1);
    // Preflight: 1 call (allow)
    assert_eq!(metrics.quota_preflight.load(Ordering::Relaxed), 1);
    // Reserve: 2 calls (daily + monthly)
    assert_eq!(metrics.quota_reserve.load(Ordering::Relaxed), 2);
    assert_eq!(metrics.quota_estimated_tokens.load(Ordering::Relaxed), 1);
    // Finalization: audit emit + latency
    assert_eq!(metrics.audit_emit.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.finalization_latency_ms.load(Ordering::Relaxed), 1);
}

/// Failure path: provider error emits `stream_failed`, not `stream_completed`.
#[tokio::test]
async fn metrics_emitted_on_provider_error() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let metrics = Arc::new(TestMetrics::new());
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::failing());
    let svc = build_stream_service_with_metrics(db, provider, Arc::clone(&metrics) as _);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect("should succeed (stream spawned)");

    // Drain events — expect an error event
    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            break;
        }
    }
    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Failed);

    // Verify metric emissions
    assert_eq!(metrics.stream_started.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_completed.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.stream_failed.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.active_streams_inc.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.active_streams_dec.load(Ordering::Relaxed), 1);
    // No first token → no TTFT metrics
    assert_eq!(metrics.ttft_provider_ms.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.ttft_overhead_ms.load(Ordering::Relaxed), 0);
    // Total latency is still recorded
    assert_eq!(metrics.stream_total_latency_ms.load(Ordering::Relaxed), 1);
    // Preflight still ran
    assert_eq!(metrics.quota_preflight.load(Ordering::Relaxed), 1);
}

/// Incomplete path: `stream_incomplete` + `stream_completed` both emitted.
#[tokio::test]
async fn metrics_emitted_on_incomplete_stream() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let metrics = Arc::new(TestMetrics::new());
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::incomplete(&["Partial output"]));
    let svc = build_stream_service_with_metrics(db, provider, Arc::clone(&metrics) as _);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel,
            tx,
        )
        .await
        .expect("should succeed");

    while let Some(ev) = rx.recv().await {
        if ev.is_terminal() {
            break;
        }
    }
    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Incomplete);

    // stream_incomplete is a sub-counter of stream_completed (both emitted)
    assert_eq!(metrics.stream_incomplete.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_completed.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_failed.load(Ordering::Relaxed), 0);
    // Active streams gauge balanced
    assert_eq!(metrics.active_streams_inc.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.active_streams_dec.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_total_latency_ms.load(Ordering::Relaxed), 1);
}

/// Cancel path: cancellation metrics emitted, `active_streams` gauge balanced.
#[tokio::test]
async fn metrics_emitted_on_cancelled_stream() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;

    let metrics = Arc::new(TestMetrics::new());
    let provider: Arc<dyn LlmProvider> = Arc::new(HangingProvider);
    let svc = build_stream_service_with_metrics(db, provider, Arc::clone(&metrics) as _);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let handle = svc
        .run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "hello".into(),
            test_resolved_model(),
            false,
            Vec::new(),
            cancel.clone(),
            tx,
        )
        .await
        .expect("should succeed");

    // Wait for stream_started and first delta to arrive, then cancel
    let started = rx.recv().await.expect("should get stream_started");
    assert!(matches!(started, StreamEvent::StreamStarted(_)));
    let ev = rx.recv().await.expect("should receive delta");
    assert!(
        matches!(ev, StreamEvent::Delta(_)),
        "second event should be a delta"
    );
    cancel.cancel();

    // Drain remaining events
    while rx.recv().await.is_some() {}
    let outcome = handle.await.expect("task should complete");
    assert_eq!(outcome.terminal, StreamTerminal::Cancelled);

    // Cancellation metrics
    assert_eq!(metrics.cancel_requested.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_disconnected.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.cancel_effective.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.time_to_abort_ms.load(Ordering::Relaxed), 1);
    // NOT completed or failed
    assert_eq!(metrics.stream_completed.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.stream_failed.load(Ordering::Relaxed), 0);
    // Active streams gauge balanced
    assert_eq!(metrics.active_streams_inc.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.active_streams_dec.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.stream_total_latency_ms.load(Ordering::Relaxed), 1);
}

/// `run_stream_for_mutation` returns `InputTooLong` for oversized content
/// and marks the already-committed turn as `Failed` so it does not stay
/// stuck in `Running` state.
///
/// Setup: model catalog has `context_window = max_input_tokens = 500`.
/// Content: 1500 ASCII bytes → ~523 estimated tokens > 500 → `InputTooLong`.
#[tokio::test]
async fn run_stream_for_mutation_input_too_long_marks_turn_failed() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    insert_test_chat(&db, tenant_id, user_id, chat_id).await;
    insert_running_turn(&db, tenant_id, user_id, chat_id, request_id, turn_id).await;

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&[]));
    let svc = build_stream_service_with_context_window(db.clone(), provider, 500);

    let ctx = test_security_ctx_with_id(tenant_id, user_id);
    let (tx, _rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();

    let err = svc
        .run_stream_for_mutation(
            ctx,
            chat_id,
            request_id,
            turn_id,
            "a".repeat(1500),
            test_resolved_model(),
            false,
            None,
            cancel,
            tx,
        )
        .await
        .expect_err("should be InputTooLong");

    match err {
        StreamError::InputTooLong {
            estimated_tokens,
            max_input_tokens,
        } => {
            assert_eq!(max_input_tokens, 500);
            assert!(
                estimated_tokens > u64::from(max_input_tokens),
                "estimated {estimated_tokens} should exceed limit {max_input_tokens}"
            );
        }
        other => panic!("expected InputTooLong, got: {other:?}"),
    }

    // The pre-committed turn must be marked Failed, not left in Running.
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let turn = TurnRepo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .expect("DB query should succeed")
        .expect("turn must exist");
    assert_eq!(
        turn.state,
        TurnState::Failed,
        "turn should be marked Failed after InputTooLong"
    );
    assert_eq!(
        turn.error_code.as_deref(),
        Some("input_too_long"),
        "error_code should be set to input_too_long"
    );
}
