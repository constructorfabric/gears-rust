//! Thread summary outbox handler - processes `thread_summary` queue events.
//!
//! Runs as part of the outbox pipeline (leased strategy). All replicas
//! process events in parallel, partitioned by `chat_id`. No leader election needed.

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use toolkit_db::DBProvider;
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use toolkit_security::AccessScope;
use tracing::{debug, error, info, warn};

use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::repos::{SummaryFrontier, ThreadSummaryRepository, ThreadSummaryTaskPayload};
use crate::infra::db::entity::message::MessageRole;

type DbProvider = DBProvider<toolkit_db::DbError>;
type MessageRepo = crate::infra::db::repo::message_repo::MessageRepository;
type ThreadSummaryRepo = crate::infra::db::repo::thread_summary_repo::ThreadSummaryRepository;

pub struct ThreadSummaryDeps {
    pub db: Arc<DbProvider>,
    pub thread_summary_repo: Arc<ThreadSummaryRepo>,
    pub message_repo: Arc<MessageRepo>,
    pub outbox_enqueuer: Arc<dyn crate::domain::repos::OutboxEnqueuer>,
    pub metrics: Arc<dyn MiniChatMetricsPort>,
    pub provider_resolver: Arc<crate::infra::llm::provider_resolver::ProviderResolver>,
    pub model_resolver: Arc<dyn crate::domain::repos::ModelResolver>,
    pub config: crate::config::background::ThreadSummaryWorkerConfig,
}

pub struct ThreadSummaryHandler {
    deps: Arc<ThreadSummaryDeps>,
}

impl ThreadSummaryHandler {
    pub fn new(deps: Arc<ThreadSummaryDeps>) -> Self {
        Self { deps }
    }
}

#[async_trait]
impl LeasedMessageHandler for ThreadSummaryHandler {
    #[tracing::instrument(name = "worker", skip_all, fields(worker = "thread_summary"))]
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        // 1. Deserialize payload
        let payload: ThreadSummaryTaskPayload = match serde_json::from_slice(&msg.payload) {
            Ok(p) => p,
            Err(e) => {
                error!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    error = %e,
                    "thread summary: invalid payload, dead-lettering"
                );
                return MessageResult::Reject(format!("payload deserialization failed: {e}"));
            }
        };

        let base_frontier = match (
            &payload.base_frontier_created_at,
            &payload.base_frontier_message_id,
        ) {
            (Some(ca), Some(mid)) => Some(SummaryFrontier {
                created_at: *ca,
                message_id: *mid,
            }),
            (None, None) => None,
            _ => {
                error!(
                    chat_id = %payload.chat_id,
                    "thread summary: partial frontier (one field set, other missing), dead-lettering"
                );
                return MessageResult::Reject(
                    "partial base_frontier: both fields must be set or both absent".to_owned(),
                );
            }
        };

        let target_frontier = SummaryFrontier {
            created_at: payload.frozen_target_created_at,
            message_id: payload.frozen_target_message_id,
        };

        // 2. Pre-check: verify stored frontier still matches base_frontier
        let conn = match self.deps.db.conn() {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    chat_id = %payload.chat_id,
                    error = %e,
                    "thread summary: DB connection failed"
                );
                return MessageResult::Retry;
            }
        };

        let scope = AccessScope::for_tenant(payload.tenant_id);

        let current = self
            .deps
            .thread_summary_repo
            .get_latest(&conn, &scope, payload.chat_id)
            .await;

        match &current {
            Ok(Some(existing)) => {
                if base_frontier.as_ref() != Some(&existing.frontier) {
                    info!(
                        chat_id = %payload.chat_id,
                        "thread summary: frontier already advanced, skipping"
                    );
                    self.deps.metrics.record_thread_summary_cas_conflict();
                    return MessageResult::Ok;
                }
            }
            Ok(None) => {
                if base_frontier.is_some() {
                    warn!(
                        chat_id = %payload.chat_id,
                        "thread summary: expected frontier but none found, skipping"
                    );
                    return MessageResult::Ok;
                }
            }
            Err(e) => {
                warn!(
                    chat_id = %payload.chat_id,
                    error = %e,
                    "thread summary: pre-check query failed"
                );
                return MessageResult::Retry;
            }
        }

        // 3. Load messages in range
        let messages = match crate::domain::repos::MessageRepository::fetch_messages_in_range(
            self.deps.message_repo.as_ref(),
            &conn,
            &scope,
            payload.chat_id,
            base_frontier.as_ref(),
            &target_frontier,
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    chat_id = %payload.chat_id,
                    error = %e,
                    "thread summary: message fetch failed"
                );
                return MessageResult::Retry;
            }
        };

        // Note: `conn` is a pool handle released when it goes out of scope.
        // The LLM call below may take seconds, but conn is a lightweight
        // reference — actual pool slot is managed by the DB pool internals.

        if messages.is_empty() {
            debug!(
                chat_id = %payload.chat_id,
                "thread summary: no messages in range, skipping"
            );
            return MessageResult::Ok;
        }

        // 4. Generate summary via LLM
        let existing_summary = current
            .as_ref()
            .ok()
            .and_then(|c| c.as_ref())
            .map(|s| s.content.as_str());

        // 4a. Resolve model
        let model_id = if self.deps.config.summary_model_id.is_empty() {
            "gpt-4.1-mini".to_owned()
        } else {
            self.deps.config.summary_model_id.clone()
        };

        let resolved_model = match self
            .deps
            .model_resolver
            .resolve_model(
                toolkit_security::constants::DEFAULT_SUBJECT_ID,
                Some(model_id.clone()),
            )
            .await
        {
            Ok(m) => m,
            Err(e) => {
                warn!(chat_id = %payload.chat_id, error = %e, "thread summary: model resolution failed");
                self.deps.metrics.record_thread_summary_execution("retry");
                return MessageResult::Retry;
            }
        };

        // 4b. Resolve provider
        let tenant_id_str = payload.tenant_id.to_string();
        let resolved_provider = match self
            .deps
            .provider_resolver
            .resolve(&resolved_model.provider_id, Some(&tenant_id_str))
        {
            Ok(p) => p,
            Err(e) => {
                warn!(chat_id = %payload.chat_id, error = %e, "thread summary: provider resolution failed");
                self.deps.metrics.record_thread_summary_execution("retry");
                return MessageResult::Retry;
            }
        };

        // 4c. Build prompt — 3-tier cascade: model-specific → global config → default
        let system_prompt = if !resolved_model.thread_summary_prompt.is_empty() {
            resolved_model.thread_summary_prompt.clone()
        } else if !self.deps.config.summary_system_prompt.is_empty() {
            self.deps.config.summary_system_prompt.clone()
        } else {
            crate::config::background::ThreadSummaryWorkerConfig::default().summary_system_prompt
        };

        // Build full OAGW proxy path: {alias}{api_path} — same as stream flow.
        let api_path = resolved_provider
            .api_path
            .replace("{model}", &resolved_model.provider_model_id);
        let upstream_path = format!("{}{api_path}", resolved_provider.upstream_alias);

        // 4d. Call LLM with PTL retry — if context_length_exceeded, drop
        //     oldest messages and retry (up to 2 times).
        let max_ptl_retries = 2u32;
        let mut messages_for_prompt = messages.clone();
        let mut ptl_retries = 0u32;
        let content_limit = self.deps.config.message_content_limit;

        #[allow(clippy::expect_used)]
        let security_ctx = toolkit_security::SecurityContext::builder()
            .subject_tenant_id(payload.tenant_id)
            .subject_id(toolkit_security::constants::DEFAULT_SUBJECT_ID)
            .build()
            .expect("tenant SecurityContext must build");

        let response = loop {
            let user_content =
                build_summary_prompt(existing_summary, &messages_for_prompt, content_limit);

            let request = crate::infra::llm::llm_request(&resolved_model.provider_model_id)
                .system_instructions(&system_prompt)
                .message(crate::infra::llm::LlmMessage::user(&user_content))
                .max_output_tokens(u64::from(resolved_model.max_output_tokens))
                .build_non_streaming();

            let ctx = security_ctx.clone();
            match resolved_provider
                .adapter
                .complete(ctx, request, &upstream_path)
                .await
            {
                Ok(r) => break r,
                Err(e) if is_context_length_error(&e) && ptl_retries < max_ptl_retries => {
                    let drop_count = (messages_for_prompt.len().div_ceil(5)).max(2);
                    let actual_drop = drop_count.min(messages_for_prompt.len().saturating_sub(2));
                    if actual_drop == 0 {
                        warn!(chat_id = %payload.chat_id, error = %e,
                              "thread summary: prompt too long, cannot drop more messages");
                        self.deps
                            .metrics
                            .record_thread_summary_execution("provider_error");
                        self.deps.metrics.record_summary_fallback();
                        return MessageResult::Retry;
                    }
                    messages_for_prompt.drain(..actual_drop);
                    ptl_retries += 1;
                    warn!(
                        chat_id = %payload.chat_id,
                        ptl_retries,
                        dropped = actual_drop,
                        remaining = messages_for_prompt.len(),
                        "thread summary: prompt too long, dropping oldest messages and retrying"
                    );
                }
                Err(e) => {
                    warn!(chat_id = %payload.chat_id, error = %e, "thread summary: LLM call failed");
                    self.deps
                        .metrics
                        .record_thread_summary_execution("provider_error");
                    self.deps.metrics.record_summary_fallback();
                    return MessageResult::Retry;
                }
            }
        };

        let summary_text = format_summary_output(&response.content);
        if summary_text.trim().is_empty() {
            warn!(chat_id = %payload.chat_id, "thread summary: LLM returned empty summary, retrying");
            self.deps
                .metrics
                .record_thread_summary_execution("empty_summary");
            return MessageResult::Retry;
        }
        let llm_usage = response.usage;
        let token_estimate = estimate_summary_tokens(llm_usage.output_tokens, summary_text.len());

        // 5. CAS-protected atomic commit
        let deps = Arc::clone(&self.deps);
        let base_clone = base_frontier.clone();
        let target_clone = target_frontier.clone();
        let summary_clone = summary_text;
        let msg_count = messages.len();
        let payload_clone = payload.clone();
        let model_id_clone = model_id;

        let cas_result = self
            .deps
            .db
            .transaction(|tx| {
                let deps = Arc::clone(&deps);
                let base_clone = base_clone.clone();
                let target_clone = target_clone.clone();
                let summary_clone = summary_clone.clone();
                let scope = scope.clone();
                let payload_clone = payload_clone.clone();
                let model_id_clone = model_id_clone.clone();
                Box::pin(async move {
                    // 5a. Upsert summary with CAS
                    let rows = deps
                        .thread_summary_repo
                        .upsert_with_cas(
                            tx,
                            payload_clone.chat_id,
                            payload_clone.tenant_id,
                            base_clone.as_ref(),
                            &target_clone,
                            &summary_clone,
                            token_estimate,
                        )
                        .await
                        .map_err(|e| toolkit_db::DbError::Other(anyhow::anyhow!("{e}")))?;

                    if rows == 0 {
                        return Ok((false, crate::domain::repos::Wake::empty()));
                    }

                    // 5b. Mark messages as compressed
                    crate::domain::repos::MessageRepository::mark_messages_compressed(
                        deps.message_repo.as_ref(),
                        tx,
                        &scope,
                        payload_clone.chat_id,
                        base_clone.as_ref(),
                        &target_clone,
                    )
                    .await
                    .map_err(|e| toolkit_db::DbError::Other(anyhow::anyhow!("{e}")))?;

                    // 5c. Enqueue system usage event
                    let usage_event = mini_chat_sdk::UsageEvent {
                        tenant_id: payload_clone.tenant_id,
                        user_id: None,
                        chat_id: payload_clone.chat_id,
                        turn_id: None,
                        request_id: payload_clone.system_request_id,
                        effective_model: model_id_clone.clone(),
                        selected_model: model_id_clone,
                        terminal_state: "completed".to_owned(),
                        billing_outcome: "system_task".to_owned(),
                        usage: Some(mini_chat_sdk::UsageTokens {
                            input_tokens: u64::try_from(llm_usage.input_tokens.max(0)).unwrap_or(0),
                            output_tokens: u64::try_from(llm_usage.output_tokens.max(0))
                                .unwrap_or(0),
                            cache_read_input_tokens: u64::try_from(
                                llm_usage.cache_read_input_tokens.max(0),
                            )
                            .unwrap_or(0),
                            cache_write_input_tokens: u64::try_from(
                                llm_usage.cache_write_input_tokens.max(0),
                            )
                            .unwrap_or(0),
                            reasoning_tokens: u64::try_from(llm_usage.reasoning_tokens.max(0))
                                .unwrap_or(0),
                        }),
                        actual_credits_micro: 0,
                        settlement_method: "none".to_owned(),
                        policy_version_applied: 0,
                        web_search_calls: 0,
                        code_interpreter_calls: 0,
                        file_search_calls: 0,
                        timestamp: time::OffsetDateTime::now_utc(),
                        requester_type: "system".to_owned(),
                        dedupe_key: Some(format!(
                            "{}/{}/{}",
                            payload_clone.tenant_id.as_simple(),
                            "thread_summary_update",
                            payload_clone.system_request_id.as_simple(),
                        )),
                        system_task_type: Some("thread_summary_update".to_owned()),
                    };
                    let wake = deps
                        .outbox_enqueuer
                        .enqueue_usage_event(tx, usage_event)
                        .await
                        .map_err(|e| toolkit_db::DbError::Other(anyhow::anyhow!("{e}")))?;

                    Ok((true, wake))
                })
            })
            .await;

        match cas_result {
            Ok((true, wake)) => {
                wake.fire();
                self.deps.metrics.record_thread_summary_execution("success");
                info!(
                    chat_id = %payload.chat_id,
                    messages_compressed = msg_count,
                    "thread summary: committed successfully"
                );
                MessageResult::Ok
            }
            Ok((false, _)) => {
                self.deps.metrics.record_thread_summary_cas_conflict();
                info!(
                    chat_id = %payload.chat_id,
                    "thread summary: CAS conflict on commit, skipping"
                );
                MessageResult::Ok
            }
            Err(e) => {
                warn!(
                    chat_id = %payload.chat_id,
                    error = %e,
                    "thread summary: commit failed"
                );
                self.deps.metrics.record_thread_summary_execution("retry");
                MessageResult::Retry
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Prompt construction & output formatting
// ════════════════════════════════════════════════════════════════════════════

const ANALYSIS_INSTRUCTION: &str = "\
Before providing your final summary, wrap your analysis in <analysis> tags. In your analysis:
1. Chronologically review each exchange, identifying:
   - The user's requests and questions
   - Key decisions, answers, and information shared
   - Any follow-up actions or commitments
   - Specific names, dates, numbers, URLs, or references mentioned
2. Verify accuracy and completeness.

Your summary MUST include these sections:

1. Conversation Purpose: The user's primary goals and recurring themes
2. Key Information Exchanged: Important facts, decisions, recommendations, and answers
3. User Requests and Preferences: All explicit user requests, stated preferences, and corrections
4. Open Items: Any unresolved questions, wake actions, or things the user asked to revisit
5. Current Topic: What was being discussed most recently, with enough detail to continue naturally

Respond with an <analysis> block followed by a <summary> block.";

/// Build the user-message prompt for summary generation.
///
/// Two variants:
/// - **Full** (`existing_summary` is `None`): summarize the entire conversation.
/// - **Partial** (`existing_summary` is `Some`): incorporate existing summary with new messages.
fn build_summary_prompt(
    existing_summary: Option<&str>,
    messages: &[crate::infra::db::entity::message::Model],
    message_content_limit: usize,
) -> String {
    let mut prompt = String::new();

    if let Some(prev) = existing_summary {
        prompt.push_str("The existing summary below covers the earlier conversation. Incorporate it with the new messages into a single updated summary.\n\n");
        prompt.push_str("IMPORTANT: Keep the summary concise. If the combined information is too large, prioritize: current topic and recent decisions > user preferences and corrections > older facts. Compress or drop the least relevant older details rather than letting the summary grow unboundedly.\n\n");
        prompt.push_str("<existing_summary>\n");
        prompt.push_str(prev);
        prompt.push_str("\n</existing_summary>\n\n");
        prompt.push_str("New messages to incorporate:\n\n");
    } else {
        prompt.push_str("Summarize the following conversation:\n\n");
    }

    for msg in messages {
        // Skip system messages — they contain internal prompts that should
        // not leak into the stored summary (matches context_assembly behavior).
        if matches!(msg.role, MessageRole::System) {
            continue;
        }
        let role = match msg.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::System => unreachable!(),
        };
        let content =
            if message_content_limit > 0 && msg.content.chars().count() > message_content_limit {
                let truncated: String = msg.content.chars().take(message_content_limit).collect();
                format!("{truncated}...")
            } else {
                msg.content.clone()
            };
        prompt.push_str(role);
        prompt.push_str(": ");
        prompt.push_str(&content);
        prompt.push_str("\n\n");
    }

    prompt.push_str(ANALYSIS_INSTRUCTION);
    prompt
}

#[allow(clippy::expect_used)]
static ANALYSIS_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?s)<analysis>.*?</analysis>").expect("valid regex"));
#[allow(clippy::expect_used)]
static SUMMARY_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?s)<summary>(.*?)</summary>").expect("valid regex"));
#[allow(clippy::expect_used)]
static MULTI_NEWLINE_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\n{3,}").expect("valid regex"));

/// Strip the `<analysis>` drafting scratchpad and extract `<summary>` content.
///
/// The `<analysis>` block improves summary quality by forcing step-by-step
/// reasoning, but wastes tokens when stored. Only `<summary>` is persisted.
fn format_summary_output(raw: &str) -> String {
    let without_analysis = ANALYSIS_RE.replace(raw, "");

    if let Some(caps) = SUMMARY_RE.captures(&without_analysis) {
        let content = caps.get(1).map_or("", |m| m.as_str()).trim();
        MULTI_NEWLINE_RE.replace_all(content, "\n\n").into_owned()
    } else {
        // Graceful fallback: no <summary> tags
        let cleaned = ANALYSIS_RE.replace(raw, "");
        let trimmed = cleaned.trim();
        // Reject if residual markup tags remain (malformed LLM output)
        if trimmed.contains("<analysis") || trimmed.contains("<summary") {
            return String::new(); // empty → triggers retry upstream
        }
        MULTI_NEWLINE_RE.replace_all(trimmed, "\n\n").into_owned()
    }
}

/// Derive a token estimate for the stored summary.
/// Prefers actual `output_tokens` from the provider; falls back to `bytes/4`.
fn estimate_summary_tokens(output_tokens: i64, summary_byte_len: usize) -> i32 {
    if output_tokens > 0 {
        i32::try_from(output_tokens).unwrap_or(i32::MAX)
    } else {
        i32::try_from(summary_byte_len.div_ceil(4)).unwrap_or(i32::MAX)
    }
}

/// Check if an LLM error indicates the input exceeded the model's context length.
fn is_context_length_error(e: &crate::infra::llm::LlmProviderError) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("context_length_exceeded")
        || msg.contains("maximum context length")
        || msg.contains("token limit")
        || msg.contains("too many tokens")
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_db::outbox::LeasedMessageHandler;

    // `rejects_invalid_payload` removed — superseded by `e2e_handler_rejects_invalid_payload`
    // which exercises the actual handler's Reject branch.

    #[test]
    fn placeholder_summary_builds_correctly() {
        use crate::infra::db::entity::message::Model;
        use time::OffsetDateTime;
        use uuid::Uuid;

        let msg = Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            chat_id: Uuid::new_v4(),
            request_id: Some(Uuid::new_v4()),
            role: MessageRole::User,
            content: "Hello world".to_owned(),
            content_type: "text/plain".to_owned(),
            token_estimate: 5,
            provider_response_id: None,
            request_kind: None,
            features_used: serde_json::json!([]),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
            reasoning_tokens: 0,
            model: None,
            is_compressed: false,
            created_at: OffsetDateTime::now_utc(),
            deleted_at: None,
        };

        let summary = build_summary_prompt(None, &[msg], 4000);
        assert!(summary.contains("User: Hello world"));
    }

    #[test]
    fn token_estimate_prefers_output_tokens() {
        assert_eq!(estimate_summary_tokens(250, 1000), 250);
    }

    #[test]
    fn token_estimate_falls_back_to_len_div_4() {
        // 200 bytes / 4 = 50 tokens
        assert_eq!(estimate_summary_tokens(0, 200), 50);
    }

    // ── E2E tests: full handler pipeline with real DB ──────────────────

    /// Insert a message with controllable content and `created_at` for e2e tests.
    async fn insert_message(
        db: &toolkit_db::Db,
        tenant_id: uuid::Uuid,
        chat_id: uuid::Uuid,
        role: MessageRole,
        content: &str,
        created_at: time::OffsetDateTime,
    ) -> uuid::Uuid {
        use crate::infra::db::entity::message::{
            ActiveModel as MessageAM, Entity as MessageEntity,
        };
        use sea_orm::Set;

        let msg_id = uuid::Uuid::new_v4();
        let am = MessageAM {
            id: Set(msg_id),
            tenant_id: Set(tenant_id),
            chat_id: Set(chat_id),
            request_id: Set(None),
            role: Set(role),
            content: Set(content.to_owned()),
            content_type: Set("text".to_owned()),
            token_estimate: Set(1),
            provider_response_id: Set(None),
            request_kind: Set(None),
            features_used: Set(serde_json::json!([])),
            input_tokens: Set(0),
            output_tokens: Set(0),
            cache_read_input_tokens: Set(0),
            cache_write_input_tokens: Set(0),
            reasoning_tokens: Set(0),
            model: Set(None),
            is_compressed: Set(false),
            created_at: Set(created_at),
            deleted_at: Set(None),
        };
        let conn = db.conn().unwrap();
        toolkit_db::secure::secure_insert::<MessageEntity>(
            am,
            &toolkit_security::AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("insert test message");
        msg_id
    }

    /// Insert a chat row for e2e tests.
    async fn insert_chat(db: &toolkit_db::Db, tenant_id: uuid::Uuid, chat_id: uuid::Uuid) {
        use crate::infra::db::entity::chat::{ActiveModel as ChatAM, Entity as ChatEntity};
        use sea_orm::Set;
        let now = time::OffsetDateTime::now_utc();
        let am = ChatAM {
            id: Set(chat_id),
            tenant_id: Set(tenant_id),
            user_id: Set(uuid::Uuid::new_v4()),
            model: Set("gpt-5.2".to_owned()),
            title: Set(Some("test".to_owned())),
            is_temporary: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
        };
        let conn = db.conn().unwrap();
        toolkit_db::secure::secure_insert::<ChatEntity>(
            am,
            &toolkit_security::AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("insert chat");
    }

    /// Build a valid outbox message from a `ThreadSummaryTaskPayload`.
    fn make_outbox_msg(payload: &ThreadSummaryTaskPayload) -> OutboxMessage {
        OutboxMessage {
            partition_id: 0,
            seq: 1,
            payload: serde_json::to_vec(payload).unwrap(),
            payload_type: "application/json".to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0i16,
        }
    }

    /// Build `ThreadSummaryDeps` with real DB for e2e tests.
    /// Model/provider resolution will fail (no real providers), so these tests
    /// validate the handler's DB + CAS logic, not the LLM call path.
    async fn make_e2e_deps() -> (Arc<ThreadSummaryDeps>, toolkit_db::Db) {
        use crate::domain::service::test_helpers;
        let db = test_helpers::inmem_db().await;
        let db_provider = test_helpers::mock_db_provider(db.clone());
        let deps = Arc::new(ThreadSummaryDeps {
            db: db_provider,
            thread_summary_repo: Arc::new(
                crate::infra::db::repo::thread_summary_repo::ThreadSummaryRepository,
            ),
            message_repo: Arc::new(
                crate::infra::db::repo::message_repo::MessageRepository::new(
                    toolkit_db::odata::LimitCfg {
                        default: 20,
                        max: 100,
                    },
                ),
            ),
            outbox_enqueuer: Arc::new(test_helpers::RecordingOutboxEnqueuer::new()),
            metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
            provider_resolver: Arc::new(
                crate::infra::llm::provider_resolver::ProviderResolver::empty(),
            ),
            model_resolver: Arc::new(test_helpers::MockModelResolver::default()),
            config: crate::config::background::ThreadSummaryWorkerConfig::default(),
        });
        (deps, db)
    }

    #[tokio::test]
    async fn e2e_handler_rejects_invalid_payload() {
        let (deps, _db) = make_e2e_deps().await;
        let handler = ThreadSummaryHandler::new(deps);
        let msg = OutboxMessage {
            partition_id: 0,
            seq: 1,
            payload: b"not json".to_vec(),
            payload_type: "application/json".to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0i16,
        };
        let result = handler.handle(&msg).await;
        assert!(matches!(result, MessageResult::Reject(_)));
    }

    #[tokio::test]
    async fn e2e_handler_skips_empty_message_range() {
        let (deps, db) = make_e2e_deps().await;
        let tenant_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();

        insert_chat(&db, tenant_id, chat_id).await;

        let now = time::OffsetDateTime::now_utc();
        // Create payload with a target frontier but no messages in range
        let payload = ThreadSummaryTaskPayload {
            tenant_id,
            chat_id,
            system_request_id: uuid::Uuid::new_v4(),
            system_task_type: "thread_summary_update".to_owned(),
            base_frontier_created_at: None,
            base_frontier_message_id: None,
            frozen_target_created_at: now,
            frozen_target_message_id: uuid::Uuid::new_v4(),
        };

        let handler = ThreadSummaryHandler::new(deps);
        let result = handler.handle(&make_outbox_msg(&payload)).await;
        // No messages → should succeed (skip)
        assert!(matches!(result, MessageResult::Ok));
    }

    #[tokio::test]
    async fn e2e_payload_serialization_roundtrip() {
        let now = time::OffsetDateTime::now_utc();
        let payload = ThreadSummaryTaskPayload {
            tenant_id: uuid::Uuid::new_v4(),
            chat_id: uuid::Uuid::new_v4(),
            system_request_id: uuid::Uuid::new_v4(),
            system_task_type: "thread_summary_update".to_owned(),
            base_frontier_created_at: Some(now),
            base_frontier_message_id: Some(uuid::Uuid::new_v4()),
            frozen_target_created_at: now,
            frozen_target_message_id: uuid::Uuid::new_v4(),
        };

        let json = serde_json::to_vec(&payload).unwrap();
        let deserialized: ThreadSummaryTaskPayload = serde_json::from_slice(&json).unwrap();
        assert_eq!(payload.tenant_id, deserialized.tenant_id);
        assert_eq!(payload.chat_id, deserialized.chat_id);
        assert_eq!(payload.system_request_id, deserialized.system_request_id);
        assert_eq!(payload.system_task_type, deserialized.system_task_type);
        assert_eq!(
            payload.base_frontier_created_at,
            deserialized.base_frontier_created_at
        );
        assert_eq!(
            payload.base_frontier_message_id,
            deserialized.base_frontier_message_id
        );
        assert_eq!(
            payload.frozen_target_created_at,
            deserialized.frozen_target_created_at
        );
        assert_eq!(
            payload.frozen_target_message_id,
            deserialized.frozen_target_message_id
        );
    }

    #[tokio::test]
    async fn e2e_repo_get_latest_returns_none_when_empty() {
        let db = crate::domain::service::test_helpers::inmem_db().await;
        let tenant_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();

        insert_chat(&db, tenant_id, chat_id).await;

        let repo = crate::infra::db::repo::thread_summary_repo::ThreadSummaryRepository;
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(tenant_id);
        let result = crate::domain::repos::ThreadSummaryRepository::get_latest(
            &repo, &conn, &scope, chat_id,
        )
        .await
        .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn e2e_repo_upsert_first_summary_and_get_latest() {
        let db = crate::domain::service::test_helpers::inmem_db().await;
        let tenant_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();

        insert_chat(&db, tenant_id, chat_id).await;

        let now = time::OffsetDateTime::now_utc();
        let frontier = SummaryFrontier {
            created_at: now,
            message_id: uuid::Uuid::new_v4(),
        };

        let repo = crate::infra::db::repo::thread_summary_repo::ThreadSummaryRepository;
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(tenant_id);

        // Insert first summary
        let rows = crate::domain::repos::ThreadSummaryRepository::upsert_with_cas(
            &repo,
            &conn,
            chat_id,
            tenant_id,
            None,
            &frontier,
            "test summary",
            42,
        )
        .await
        .unwrap();
        assert_eq!(rows, 1);

        // Read it back
        let summary = crate::domain::repos::ThreadSummaryRepository::get_latest(
            &repo, &conn, &scope, chat_id,
        )
        .await
        .unwrap()
        .expect("summary should exist");
        assert_eq!(summary.content, "test summary");
        assert_eq!(summary.frontier, frontier);
        assert_eq!(summary.token_estimate, 42);
    }

    #[tokio::test]
    async fn e2e_repo_upsert_cas_conflict_on_first_summary() {
        let db = crate::domain::service::test_helpers::inmem_db().await;
        let tenant_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();

        insert_chat(&db, tenant_id, chat_id).await;

        let now = time::OffsetDateTime::now_utc();
        let frontier = SummaryFrontier {
            created_at: now,
            message_id: uuid::Uuid::new_v4(),
        };

        let repo = crate::infra::db::repo::thread_summary_repo::ThreadSummaryRepository;
        let conn = db.conn().unwrap();

        // First insert succeeds
        let rows = crate::domain::repos::ThreadSummaryRepository::upsert_with_cas(
            &repo, &conn, chat_id, tenant_id, None, &frontier, "first", 10,
        )
        .await
        .unwrap();
        assert_eq!(rows, 1);

        // Second insert with base=None conflicts (UNIQUE on chat_id)
        let rows = crate::domain::repos::ThreadSummaryRepository::upsert_with_cas(
            &repo, &conn, chat_id, tenant_id, None, &frontier, "second", 20,
        )
        .await
        .unwrap();
        assert_eq!(rows, 0, "CAS should fail - row already exists");
    }

    #[tokio::test]
    async fn e2e_repo_advance_frontier_succeeds() {
        let db = crate::domain::service::test_helpers::inmem_db().await;
        let tenant_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();

        insert_chat(&db, tenant_id, chat_id).await;

        let now = time::OffsetDateTime::now_utc();
        let frontier_a = SummaryFrontier {
            created_at: now,
            message_id: uuid::Uuid::new_v4(),
        };
        let frontier_b = SummaryFrontier {
            created_at: now + time::Duration::seconds(10),
            message_id: uuid::Uuid::new_v4(),
        };

        let repo = crate::infra::db::repo::thread_summary_repo::ThreadSummaryRepository;
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(tenant_id);

        // Insert first
        crate::domain::repos::ThreadSummaryRepository::upsert_with_cas(
            &repo,
            &conn,
            chat_id,
            tenant_id,
            None,
            &frontier_a,
            "first",
            10,
        )
        .await
        .unwrap();

        // Advance frontier
        let rows = crate::domain::repos::ThreadSummaryRepository::upsert_with_cas(
            &repo,
            &conn,
            chat_id,
            tenant_id,
            Some(&frontier_a),
            &frontier_b,
            "second",
            20,
        )
        .await
        .unwrap();
        assert_eq!(rows, 1, "CAS should succeed - frontier matches");

        // Verify
        let summary = crate::domain::repos::ThreadSummaryRepository::get_latest(
            &repo, &conn, &scope, chat_id,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(summary.content, "second");
        assert_eq!(summary.frontier, frontier_b);
    }

    #[tokio::test]
    async fn e2e_repo_advance_frontier_cas_conflict() {
        let db = crate::domain::service::test_helpers::inmem_db().await;
        let tenant_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();

        insert_chat(&db, tenant_id, chat_id).await;

        let now = time::OffsetDateTime::now_utc();
        let frontier_a = SummaryFrontier {
            created_at: now,
            message_id: uuid::Uuid::new_v4(),
        };
        let frontier_b = SummaryFrontier {
            created_at: now + time::Duration::seconds(10),
            message_id: uuid::Uuid::new_v4(),
        };
        let stale_frontier = SummaryFrontier {
            created_at: now - time::Duration::seconds(10),
            message_id: uuid::Uuid::new_v4(),
        };

        let repo = crate::infra::db::repo::thread_summary_repo::ThreadSummaryRepository;
        let conn = db.conn().unwrap();

        // Insert first
        crate::domain::repos::ThreadSummaryRepository::upsert_with_cas(
            &repo,
            &conn,
            chat_id,
            tenant_id,
            None,
            &frontier_a,
            "first",
            10,
        )
        .await
        .unwrap();

        // Try to advance with stale base frontier
        let rows = crate::domain::repos::ThreadSummaryRepository::upsert_with_cas(
            &repo,
            &conn,
            chat_id,
            tenant_id,
            Some(&stale_frontier),
            &frontier_b,
            "stale",
            30,
        )
        .await
        .unwrap();
        assert_eq!(rows, 0, "CAS should fail - frontier doesn't match");
    }

    #[tokio::test]
    async fn e2e_mark_messages_compressed() {
        let db = crate::domain::service::test_helpers::inmem_db().await;
        let tenant_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();

        insert_chat(&db, tenant_id, chat_id).await;

        let base_time = time::OffsetDateTime::now_utc();
        let _msg1 = insert_message(
            &db,
            tenant_id,
            chat_id,
            MessageRole::User,
            "msg1",
            base_time,
        )
        .await;
        let msg2 = insert_message(
            &db,
            tenant_id,
            chat_id,
            MessageRole::Assistant,
            "msg2",
            base_time + time::Duration::seconds(1),
        )
        .await;
        let msg3 = insert_message(
            &db,
            tenant_id,
            chat_id,
            MessageRole::User,
            "msg3",
            base_time + time::Duration::seconds(2),
        )
        .await;

        let target = SummaryFrontier {
            created_at: base_time + time::Duration::seconds(1),
            message_id: msg2,
        };

        let repo = crate::infra::db::repo::message_repo::MessageRepository::new(
            toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            },
        );
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(tenant_id);

        // Mark messages up to msg2 as compressed
        let rows = crate::domain::repos::MessageRepository::mark_messages_compressed(
            &repo, &conn, &scope, chat_id, None, &target,
        )
        .await
        .unwrap();
        assert_eq!(rows, 2, "msg1 and msg2 should be compressed");

        // Verify msg3 is NOT compressed
        let remaining = crate::domain::repos::MessageRepository::fetch_messages_in_range(
            &repo,
            &conn,
            &scope,
            chat_id,
            Some(&target),
            &SummaryFrontier {
                created_at: base_time + time::Duration::seconds(10),
                message_id: uuid::Uuid::max(),
            },
        )
        .await
        .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, msg3);
    }

    #[test]
    fn placeholder_summary_appends_to_existing() {
        use crate::infra::db::entity::message::Model;
        use time::OffsetDateTime;
        use uuid::Uuid;

        let msg = Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            chat_id: Uuid::new_v4(),
            request_id: Some(Uuid::new_v4()),
            role: MessageRole::Assistant,
            content: "I can help with that.".to_owned(),
            content_type: "text/plain".to_owned(),
            token_estimate: 10,
            provider_response_id: None,
            request_kind: None,
            features_used: serde_json::json!([]),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
            reasoning_tokens: 0,
            model: None,
            is_compressed: false,
            created_at: OffsetDateTime::now_utc(),
            deleted_at: None,
        };

        let summary = build_summary_prompt(Some("Previous summary"), &[msg], 4000);
        assert!(summary.contains("Previous summary"));
        assert!(summary.contains("existing_summary"));
        assert!(summary.contains("Assistant: I can help with that."));
    }

    // ── format_summary_output tests ─────────────────────────────────

    #[test]
    fn format_summary_output_strips_analysis_extracts_summary() {
        let raw = "<analysis>\nThinking about the conversation...\n</analysis>\n\n<summary>\n1. Purpose: Testing\n2. Key Info: Works\n</summary>";
        let result = format_summary_output(raw);
        assert!(!result.contains("<analysis>"));
        assert!(!result.contains("Thinking about"));
        assert!(result.contains("1. Purpose: Testing"));
        assert!(result.contains("2. Key Info: Works"));
        assert!(!result.contains("<summary>"));
    }

    #[test]
    fn format_summary_output_fallback_when_no_tags() {
        let raw = "Just a plain text summary without XML tags.";
        let result = format_summary_output(raw);
        assert_eq!(result, "Just a plain text summary without XML tags.");
    }

    #[test]
    fn format_summary_output_handles_empty_analysis() {
        let raw = "<analysis></analysis>\n<summary>Clean result</summary>";
        let result = format_summary_output(raw);
        assert_eq!(result, "Clean result");
    }

    #[test]
    fn format_summary_output_collapses_excessive_newlines() {
        let raw = "<summary>Line 1\n\n\n\n\nLine 2</summary>";
        let result = format_summary_output(raw);
        assert_eq!(result, "Line 1\n\nLine 2");
    }

    // ── build_summary_prompt tests ──────────────────────────────────

    #[test]
    fn build_summary_prompt_full_compact_no_existing() {
        let msg = test_message(MessageRole::User, "What is Rust?");
        let prompt = build_summary_prompt(None, &[msg], 4000);
        assert!(prompt.starts_with("Summarize the following conversation:"));
        assert!(prompt.contains("User: What is Rust?"));
        assert!(prompt.contains("<analysis>"));
        assert!(prompt.contains("<summary>"));
        assert!(!prompt.contains("existing_summary"));
    }

    #[test]
    fn build_summary_prompt_partial_compact_with_existing() {
        let msg = test_message(MessageRole::Assistant, "Rust is a systems language.");
        let prompt = build_summary_prompt(Some("Prior context here"), &[msg], 4000);
        assert!(prompt.contains("existing_summary"));
        assert!(prompt.contains("Prior context here"));
        assert!(prompt.contains("New messages to incorporate:"));
        assert!(prompt.contains("Assistant: Rust is a systems language."));
    }

    #[test]
    fn build_summary_prompt_respects_content_limit() {
        let long_content = "x".repeat(200);
        let msg = test_message(MessageRole::User, &long_content);
        let prompt = build_summary_prompt(None, &[msg], 50);
        // Should be truncated to 50 chars + "..."
        assert!(prompt.contains(&"x".repeat(50)));
        assert!(prompt.contains("..."));
        assert!(!prompt.contains(&"x".repeat(200)));
    }

    #[test]
    fn build_summary_prompt_no_truncation_when_limit_zero() {
        let long_content = "y".repeat(10000);
        let msg = test_message(MessageRole::User, &long_content);
        let prompt = build_summary_prompt(None, &[msg], 0);
        assert!(prompt.contains(&long_content));
    }

    // ── is_context_length_error tests ───────────────────────────────

    #[test]
    fn is_context_length_error_matches_known_patterns() {
        use crate::infra::llm::LlmProviderError;

        let e1 = LlmProviderError::ProviderError {
            code: "context_length_exceeded".into(),
            message: "too many tokens".into(),
            raw_detail: None,
        };
        assert!(is_context_length_error(&e1));

        let e2 = LlmProviderError::ProviderError {
            code: "invalid_request".into(),
            message: "maximum context length exceeded".into(),
            raw_detail: None,
        };
        assert!(is_context_length_error(&e2));

        let e3 = LlmProviderError::ProviderError {
            code: "rate_limit".into(),
            message: "rate limited".into(),
            raw_detail: None,
        };
        assert!(!is_context_length_error(&e3));
    }

    fn test_message(role: MessageRole, content: &str) -> crate::infra::db::entity::message::Model {
        crate::infra::db::entity::message::Model {
            id: uuid::Uuid::new_v4(),
            tenant_id: uuid::Uuid::new_v4(),
            chat_id: uuid::Uuid::new_v4(),
            request_id: Some(uuid::Uuid::new_v4()),
            role,
            content: content.to_owned(),
            content_type: "text/plain".to_owned(),
            token_estimate: 0,
            provider_response_id: None,
            request_kind: None,
            features_used: serde_json::json!([]),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
            reasoning_tokens: 0,
            model: None,
            is_compressed: false,
            created_at: time::OffsetDateTime::now_utc(),
            deleted_at: None,
        }
    }
}
