#![allow(
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::redundant_closure_for_method_calls,
    clippy::cast_precision_loss,
    clippy::non_ascii_literal
)]

use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use tracing::{Instrument, debug, info, warn};

use crate::domain::llm::ToolPhase;
use crate::domain::ports::FunctionTool;
use crate::domain::ports::knowledge_retriever::{
    KnowledgeRetriever, RetrievalRequest, RetrievedChunk,
};
use crate::domain::ports::metric_labels::{stage, trigger};
use crate::domain::repos::{MessageRepository, ToolCallType, TurnRepository};
use crate::domain::stream_events::{DoneData, ErrorData, StreamEvent};
use crate::infra::db::entity::chat_turn::TurnState;
use crate::infra::llm::{
    ClientSseEvent, LlmMessage, LlmProvider, LlmProviderError, LlmRequestBuilder, LlmTool,
    RequestMetadata, RequestType, TerminalOutcome,
};

use toolkit_macros::domain_model;

use super::types::{
    ActiveStreamGuard, FinalizationCtx, PROGRESS_UPDATE_INTERVAL, StreamOutcome, StreamTerminal,
    determine_features, normalize_error,
};

/// Parameters for knowledge search (RAG) within the agentic loop.
#[domain_model]
pub(super) struct KnowledgeSearchParams {
    pub retriever: Arc<dyn KnowledgeRetriever>,
    pub vector_store_id: String,
    /// Pre-resolved OAGW upstream alias for the knowledge provider.
    pub upstream_alias: String,
    pub api_version: String,
    pub top_k: usize,
    pub max_calls: u32,
    /// Maximum characters kept per chunk after post-processing (text truncation).
    pub max_chunk_chars: usize,
    /// When `true`, format chunks as a JSON array of `search_result` blocks
    /// (required by Anthropic Messages API for citations). When `false`, use
    /// plain `[SOURCE_N]` text labels (OpenAI / Azure providers).
    pub use_search_result_blocks: bool,
}

/// Model and provider configuration for a single provider task invocation.
#[domain_model]
pub(super) struct ProviderTaskConfig {
    pub llm: Arc<dyn LlmProvider>,
    pub upstream_alias: String,
    pub messages: Vec<LlmMessage>,
    pub system_instructions: Option<String>,
    pub tools: Vec<LlmTool>,
    pub model: String,
    pub provider_model_id: String,
    pub max_output_tokens: u32,
    pub max_tool_calls: u32,
    pub web_search_max_calls: u32,
    pub code_interpreter_max_calls: u32,
    pub api_params: mini_chat_sdk::ModelApiParams,
    pub provider_file_id_map: std::collections::HashMap<String, crate::domain::llm::AttachmentRef>,
    /// `provider_file_id → anthropic_file_id` lookup for chat attachments
    /// uploaded to Anthropic Files API. Forwarded to the Anthropic adapter
    /// via `LlmRequest::anthropic_file_ids`. Empty for non-Anthropic chats.
    pub anthropic_file_ids: std::collections::HashMap<String, String>,
    /// Knowledge search parameters; `None` when the feature is disabled.
    pub knowledge_search: Option<KnowledgeSearchParams>,
    /// Custom function tools enabled for this request, keyed by tool name.
    /// The agentic loop dispatches `function_call`s whose name matches a key
    /// here. Empty when no custom tools are enabled for the model.
    pub function_tools: std::collections::HashMap<String, Arc<dyn FunctionTool>>,
}

/// All five terminal paths (provider done, incomplete, provider error,
/// client disconnect, pre-stream error) route through `finalize_turn_cas()`.
/// SSE terminal events (Done/Error) are emitted only after the CAS winner
/// commits the transaction (D3).
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::let_underscore_must_use,
    clippy::cast_possible_truncation
)]
pub(super) fn spawn_provider_task<TR: TurnRepository + 'static, MR: MessageRepository + 'static>(
    ctx: SecurityContext,
    config: ProviderTaskConfig,
    cancel: CancellationToken,
    tx: mpsc::Sender<StreamEvent>,
    fin_ctx: Option<FinalizationCtx<TR, MR>>,
) -> tokio::task::JoinHandle<StreamOutcome> {
    let ProviderTaskConfig {
        llm,
        upstream_alias,
        messages,
        system_instructions,
        tools,
        model,
        provider_model_id,
        max_output_tokens,
        max_tool_calls,
        web_search_max_calls,
        code_interpreter_max_calls,
        api_params,
        provider_file_id_map,
        anthropic_file_ids,
        knowledge_search,
        function_tools,
    } = config;

    let span = if let Some(ref fctx) = fin_ctx {
        tracing::info_span!(
            "provider_stream",
            chat_id = %fctx.chat_id,
            turn_request_id = %fctx.request_id,
            turn_id = %fctx.turn_id,
            model = %model,
        )
    } else {
        tracing::info_span!("provider_stream", model = %model)
    };

    tokio::spawn(async move {
        let stream_start = std::time::Instant::now();
        let mut first_token_time: Option<std::time::Duration> = None;

        // ── Metrics: stream started + active gauge ──
        // ActiveStreamGuard ensures decrement on every exit path (Drop-based).
        let _stream_guard = if let Some(ref fctx) = fin_ctx {
            fctx.metrics
                .record_stream_started(&fctx.provider_id, &fctx.effective_model);
            fctx.metrics.increment_active_streams();
            Some(ActiveStreamGuard(Arc::clone(&fctx.metrics)))
        } else {
            None
        };

        // ── Agentic-level mutable state (persists across search_knowledge iterations) ──
        let mut accumulated_text = String::new();
        let mut cancelled = false;
        let mut web_search_call_count: u32 = 0;
        let mut web_search_completed_count: u32 = 0;
        let mut code_interpreter_call_count: u32 = 0;
        let mut code_interpreter_completed_count: u32 = 0;
        // raw_input_items grows with each search_knowledge call/output pair.
        let mut raw_input_items: Vec<serde_json::Value> = Vec::new();
        let mut knowledge_call_count: u32 = 0;
        // Per-custom-tool call counts (keyed by tool name) for generic
        // function-tool dispatch. Mirrors knowledge_call_count's soft-limit policy.
        let mut function_tool_calls: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        // Hard cap on agentic-loop iterations. Without it, a model that keeps
        // emitting `search_knowledge` after the soft per-message limit fires
        // would loop forever (each iteration injects another "limit reached"
        // notice but never terminates). The cap is `max_calls + 2`: the
        // searches themselves, plus one buffer iteration so the model can
        // summarise after the soft notice, plus one more in case the model
        // ignores the notice once. Iterations beyond that are forced into
        // a `Failed` terminal via `agentic_iterations_exceeded` below.
        //
        // When neither knowledge_search nor any function tool is enabled the
        // loop body always returns inside the first iteration (any ToolUse
        // falls through to `unexpected_tool_use`), so the cap is effectively 1.
        //
        // Custom function tools add their own per-message budgets on top, so the
        // model can call them up to their limits before the cap forces a Failed
        // terminal via `agentic_iterations_exceeded` below.
        let function_tools_budget: u32 = function_tools
            .values()
            .map(|t| t.max_calls())
            .fold(0u32, u32::saturating_add);
        let max_agentic_iterations: u32 = knowledge_search
            .as_ref()
            .map_or(0, |ks| ks.max_calls)
            .saturating_add(function_tools_budget)
            .saturating_add(2)
            .max(1);
        let mut agentic_iteration: u32 = 0;

        'agentic: loop {
            agentic_iteration = agentic_iteration.saturating_add(1);
            if agentic_iteration > max_agentic_iterations {
                warn!(
                    agentic_iteration,
                    max_agentic_iterations,
                    knowledge_call_count,
                    "agentic loop iteration cap exceeded; finalizing as failed"
                );
                let code = "agentic_iterations_exceeded".to_owned();
                let message = "Model exceeded the maximum number of tool-use iterations \
                               for this message"
                    .to_owned();
                if let Some(ref fctx) = fin_ctx {
                    let elapsed = stream_start.elapsed();
                    let finput = fctx.to_finalization_input(
                        TurnState::Failed,
                        &accumulated_text,
                        None,
                        Some(code.clone()),
                        None,
                        None,
                        web_search_completed_count,
                        code_interpreter_completed_count,
                        knowledge_call_count,
                        first_token_time.map(|d| d.as_millis() as u64),
                        Some(elapsed.as_millis() as u64),
                    );
                    match fctx.finalization_svc.finalize_turn_cas(finput).await {
                        Ok(outcome) if outcome.won_cas => {
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                        Ok(_) => {}
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on agentic iteration cap");
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                    }
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_failed(
                        &fctx.provider_id,
                        &fctx.effective_model,
                        &code,
                    );
                    fctx.metrics.record_stream_total_latency_ms(
                        &fctx.provider_id,
                        &fctx.effective_model,
                        ms,
                    );
                } else {
                    let _ = tx
                        .send(StreamEvent::Error(ErrorData {
                            code: code.clone(),
                            message,
                        }))
                        .await;
                }
                let has_partial = !accumulated_text.is_empty();
                return StreamOutcome {
                    terminal: StreamTerminal::Failed,
                    accumulated_text,
                    usage: None,
                    effective_model: model,
                    error_code: Some(code),
                    provider_response_id: None,
                    provider_partial_usage: has_partial,
                };
            }

        // Build the LLM request using provider_model_id (the actual provider-facing name)
        let mut builder = LlmRequestBuilder::new(&provider_model_id)
            .messages(messages.clone())
            .max_output_tokens(u64::from(max_output_tokens))
            .max_tool_calls(max_tool_calls)
            .raw_input_items(raw_input_items.clone());
        if let Some(ref instructions) = system_instructions {
            builder = builder.system_instructions(instructions.clone());
        }
        let features = determine_features(&tools);
        for tool in &tools {
            builder = builder.tool(tool.clone());
        }
        let metadata = RequestMetadata {
            tenant_id: ctx.subject_tenant_id().to_string(),
            user_id: ctx.subject_id().to_string(),
            chat_id: fin_ctx
                .as_ref()
                .map_or_else(String::new, |f| f.chat_id.to_string()),
            request_type: RequestType::Chat,
            features,
        };
        builder = builder.metadata(metadata);

        // Forward typed model-policy API params; each adapter selects the
        // fields its protocol supports.
        builder = builder.api_params(api_params.clone());

        // Forward the Anthropic file-id substitution map so the Anthropic
        // adapter can replace primary `provider_file_id` references in image /
        // document blocks with the actual `anthropic_file_id`. Empty for
        // non-Anthropic chats — other adapters ignore the field.
        if !anthropic_file_ids.is_empty() {
            builder = builder.anthropic_file_ids(anthropic_file_ids.clone());
        }

        let request = builder.build_streaming();

        // Use a child token for the provider HTTP stream so that calling
        // provider_stream.cancel() in tool-limit-exceeded branches only stops
        // the provider without cancelling the parent token used by SseRelay.
        // Client-disconnect cancellation still propagates via the token hierarchy.
        let provider_cancel = cancel.child_token();

        // Call the provider to start streaming
        let stream_result = llm
            .stream(ctx.clone(), request, &upstream_alias, provider_cancel)
            .await;

        let mut provider_stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                // Provider failed before any events — finalize first, then emit error.
                warn!(
                    error = %e,
                    raw_detail = e.raw_detail().unwrap_or(""),
                    "LLM provider failed before stream start"
                );
                let (code, message) = normalize_error(&e);

                if let Some(ref fctx) = fin_ctx {
                    let input = fctx.to_finalization_input(
                        TurnState::Failed,
                        "",
                        None,
                        Some(code.clone()),
                        None,
                        None,
                        0,
                        0,
                        knowledge_call_count,
                        None,
                        None,
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                        Ok(_) => { /* CAS loser — no SSE emission */ }
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on pre-stream error");
                            // Still emit error so client isn't left hanging
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                    }
                } else {
                    let _ = tx
                        .send(StreamEvent::Error(ErrorData {
                            code: code.clone(),
                            message,
                        }))
                        .await;
                }

                // Metrics: pre-stream failure
                if let Some(ref fctx) = fin_ctx {
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_failed(&fctx.provider_id, &fctx.effective_model, &code);
                    fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                }

                return StreamOutcome {
                    terminal: StreamTerminal::Failed,
                    accumulated_text: String::new(),
                    usage: None,
                    effective_model: model,
                    error_code: Some(code),
                    provider_response_id: None,
                    provider_partial_usage: false,
                };
            }
        };

        // Read events from provider, translate and forward through channel
        let mut last_progress_update = std::time::Instant::now();
        // TODO(P2): web_search_call_count (Start) is used for enforcement,
        // web_search_completed_count (Done) is used for settlement. If a search
        // starts but never completes (provider error between Start/Done), the
        // daily quota under-counts by one. Acceptable for P1 since OpenAI always
        // pairs searching→completed; revisit if we add providers that don't.

        loop {
            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    debug!("stream cancelled, aborting provider");
                    if let Some(ref fctx) = fin_ctx {
                        fctx.metrics.record_cancel_requested(trigger::DISCONNECT);
                        let disconnect_stage = if first_token_time.is_none() {
                            stage::BEFORE_FIRST_TOKEN
                        } else {
                            stage::MID_STREAM
                        };
                        fctx.metrics.record_stream_disconnected(disconnect_stage);
                    }
                    provider_stream.cancel();
                    cancelled = true;
                    break;
                }

                event = provider_stream.next() => {
                    match event {
                        Some(Ok(client_event)) => {
                            let is_first_token = matches!(client_event, ClientSseEvent::Delta { .. })
                                && first_token_time.is_none();

                            if let ClientSseEvent::Delta { r#type, ref content } = client_event {
                                if first_token_time.is_none() {
                                    let ttft = stream_start.elapsed();
                                    first_token_time = Some(ttft);
                                    info!(
                                        time_to_first_token_ms = ttft.as_millis() as u64,
                                        "first token received"
                                    );
                                    if let Some(ref fctx) = fin_ctx {
                                        let ms = ttft.as_secs_f64() * 1000.0;
                                        fctx.metrics.record_ttft_provider_ms(&fctx.provider_id, &fctx.effective_model, ms);
                                    }
                                }
                                // Only accumulate visible text for DB storage;
                                // reasoning deltas are streamed to the client
                                // but excluded from the persisted content.
                                if r#type == "text" {
                                    accumulated_text.push_str(content);
                                }

                                // Throttled progress timestamp update for orphan detection.
                                // Timer resets only on success — retry sooner on transient
                                // failures to avoid stale last_progress_at triggering false
                                // orphan detection.
                                if let Some(ref fctx) = fin_ctx
                                    && last_progress_update.elapsed() >= PROGRESS_UPDATE_INTERVAL
                                {
                                    let ok = match fctx.db.conn() {
                                        Ok(conn) => {
                                            match fctx.turn_repo.update_progress_at(&conn, &fctx.scope, fctx.turn_id).await {
                                                Ok(_) => true,
                                                Err(e) => {
                                                    warn!(turn_id = %fctx.turn_id, error = %e, "failed to update progress timestamp");
                                                    false
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            warn!(turn_id = %fctx.turn_id, error = %e, "failed to get DB connection for progress update");
                                            false
                                        }
                                    };
                                    if ok {
                                        last_progress_update = std::time::Instant::now();
                                    }
                                }
                            }

                            // Track web search tool calls for per-message limit
                            if let ClientSseEvent::Tool { ref phase, name, .. } = client_event
                                && name == "web_search"
                            {
                                match phase {
                                    ToolPhase::Start => {
                                        web_search_call_count += 1;
                                        if web_search_call_count > web_search_max_calls {
                                            warn!(
                                                web_search_call_count,
                                                limit = web_search_max_calls,
                                                "web search per-message limit exceeded"
                                            );
                                            let code = "web_search_calls_exceeded".to_owned();
                                            let message = "Web search calls exceeded for this message".to_owned();

                                            // Cancel provider first so it stops executing the
                                            // over-limit tool call during the finalization await.
                                            provider_stream.cancel();

                                            // Finalize as failed, then emit error (D3)
                                            if let Some(ref fctx) = fin_ctx {
                                                let input = fctx.to_finalization_input(
                                                    TurnState::Failed,
                                                    &accumulated_text,
                                                    None,
                                                    Some(code.clone()),
                                                    None,
                                                    None,
                                                    web_search_completed_count,
                                                    code_interpreter_completed_count,
                                                    knowledge_call_count,
                                                    None,
                                                    None,
                                                );
                                                match fctx.finalization_svc.finalize_turn_cas(input).await {
                                                    Ok(outcome) if outcome.won_cas => {
                                                        let _ = tx.send(StreamEvent::Error(ErrorData {
                                                            code: code.clone(),
                                                            message,
                                                        })).await;
                                                    }
                                                    Ok(_) => {}
                                                    Err(fe) => {
                                                        warn!(error = %fe, "finalization failed on ws limit exceeded");
                                                        let _ = tx.send(StreamEvent::Error(ErrorData {
                                                            code: code.clone(),
                                                            message,
                                                        })).await;
                                                    }
                                                }
                                            } else {
                                                let _ = tx.send(StreamEvent::Error(ErrorData {
                                                    code: code.clone(),
                                                    message,
                                                })).await;
                                            }

                                            // Metrics: web search limit exceeded
                                            if let Some(ref fctx) = fin_ctx {
                                                let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                                                fctx.metrics.record_stream_failed(
                                                    &fctx.provider_id,
                                                    &fctx.effective_model,
                                                    &code,
                                                );
                                                fctx.metrics.record_stream_total_latency_ms(
                                                    &fctx.provider_id,
                                                    &fctx.effective_model,
                                                    ms,
                                                );
                                            }

                                            let has_partial = !accumulated_text.is_empty();
                                            return StreamOutcome {
                                                terminal: StreamTerminal::Failed,
                                                accumulated_text,
                                                usage: None,
                                                effective_model: model,
                                                error_code: Some(code),
                                                provider_response_id: None,
                                                provider_partial_usage: has_partial,
                                            };
                                        }
                                    }
                                    ToolPhase::Done => {
                                        web_search_completed_count += 1;
                                        if let Some(ref fctx) = fin_ctx {
                                            match fctx.db.conn() {
                                                Ok(conn) => {
                                                    if let Err(e) = fctx.turn_repo.increment_tool_calls(&conn, &fctx.scope, fctx.turn_id, ToolCallType::WebSearch).await {
                                                        warn!(turn_id = %fctx.turn_id, error = %e, "failed to persist web_search_completed_count");
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!(turn_id = %fctx.turn_id, error = %e, "failed to acquire DB connection for web_search_completed_count");
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Track code interpreter tool calls
                            if let ClientSseEvent::Tool { ref phase, name, .. } = client_event
                                && name == "code_interpreter"
                            {
                                match phase {
                                    ToolPhase::Start => {
                                        code_interpreter_call_count += 1;
                                        if code_interpreter_call_count > code_interpreter_max_calls {
                                            warn!(
                                                code_interpreter_call_count,
                                                limit = code_interpreter_max_calls,
                                                "code interpreter per-message limit exceeded"
                                            );
                                            let code = "code_interpreter_calls_exceeded".to_owned();
                                            let message = "Code interpreter calls exceeded for this message".to_owned();

                                            // Cancel provider first so it stops executing the
                                            // over-limit tool call during the finalization await.
                                            provider_stream.cancel();

                                            if let Some(ref fctx) = fin_ctx {
                                                let input = fctx.to_finalization_input(
                                                    TurnState::Failed,
                                                    &accumulated_text,
                                                    None,
                                                    Some(code.clone()),
                                                    None,
                                                    None,
                                                    web_search_completed_count,
                                                    code_interpreter_completed_count,
                                                    knowledge_call_count,
                                                    None,
                                                    None,
                                                );
                                                match fctx.finalization_svc.finalize_turn_cas(input).await {
                                                    Ok(outcome) if outcome.won_cas => {
                                                        let _ = tx.send(StreamEvent::Error(ErrorData {
                                                            code: code.clone(),
                                                            message,
                                                        })).await;
                                                    }
                                                    Ok(_) => {}
                                                    Err(fe) => {
                                                        warn!(error = %fe, "finalization failed on ci limit exceeded");
                                                        let _ = tx.send(StreamEvent::Error(ErrorData {
                                                            code: code.clone(),
                                                            message,
                                                        })).await;
                                                    }
                                                }
                                            } else {
                                                let _ = tx.send(StreamEvent::Error(ErrorData {
                                                    code: code.clone(),
                                                    message,
                                                })).await;
                                            }

                                            if let Some(ref fctx) = fin_ctx {
                                                let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                                                fctx.metrics.record_stream_failed(
                                                    &fctx.provider_id,
                                                    &fctx.effective_model,
                                                    &code,
                                                );
                                                fctx.metrics.record_stream_total_latency_ms(
                                                    &fctx.provider_id,
                                                    &fctx.effective_model,
                                                    ms,
                                                );
                                            }

                                            let has_partial = !accumulated_text.is_empty();
                                            return StreamOutcome {
                                                terminal: StreamTerminal::Failed,
                                                accumulated_text,
                                                usage: None,
                                                effective_model: model,
                                                error_code: Some(code),
                                                provider_response_id: None,
                                                provider_partial_usage: has_partial,
                                            };
                                        }
                                    }
                                    ToolPhase::Done => {
                                        code_interpreter_completed_count += 1;
                                        if let Some(ref fctx) = fin_ctx {
                                            match fctx.db.conn() {
                                                Ok(conn) => {
                                                    if let Err(e) = fctx.turn_repo.increment_tool_calls(&conn, &fctx.scope, fctx.turn_id, ToolCallType::CodeInterpreter).await {
                                                        warn!(turn_id = %fctx.turn_id, error = %e, "failed to persist code_interpreter_completed_count");
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!(turn_id = %fctx.turn_id, error = %e, "failed to acquire DB connection for code_interpreter_completed_count");
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            let stream_event = StreamEvent::from(client_event);
                            if tx.send(stream_event).await.is_err() {
                                // Receiver dropped (client disconnect handled by relay)
                                info!("channel closed (client disconnect), exiting provider task");
                                break;
                            }

                            // TTFT overhead: time from provider first-byte to channel send.
                            if is_first_token
                                && let (Some(fctx), Some(provider_ttft)) =
                                    (&fin_ctx, first_token_time)
                                {
                                    let total = stream_start.elapsed().as_secs_f64() * 1000.0;
                                    let provider_ms = provider_ttft.as_secs_f64() * 1000.0;
                                    fctx.metrics.record_ttft_overhead_ms(
                                        &fctx.provider_id,
                                        &fctx.effective_model,
                                        total - provider_ms,
                                    );
                                }
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "provider stream error");
                            let (code, message) =
                                normalize_error(&LlmProviderError::StreamError(e));

                            // Finalize first, emit error only if CAS winner (D3)
                            if let Some(ref fctx) = fin_ctx {
                                let mid_elapsed = stream_start.elapsed();
                                let input = fctx.to_finalization_input(
                                    TurnState::Failed,
                                    &accumulated_text,
                                    None,
                                    Some(code.clone()),
                                    None,
                                    None,
                                    web_search_completed_count,
                                    code_interpreter_completed_count,
                                    knowledge_call_count,
                                    first_token_time.map(|d| d.as_millis() as u64),
                                    Some(mid_elapsed.as_millis() as u64),
                                );
                                match fctx.finalization_svc.finalize_turn_cas(input).await {
                                    Ok(outcome) if outcome.won_cas => {
                                        let _ = tx
                                            .send(StreamEvent::Error(ErrorData {
                                                code: code.clone(),
                                                message,
                                            }))
                                            .await;
                                    }
                                    Ok(_) => {}
                                    Err(fe) => {
                                        warn!(error = %fe, "finalization failed on stream error");
                                        let _ = tx
                                            .send(StreamEvent::Error(ErrorData {
                                                code: code.clone(),
                                                message,
                                            }))
                                            .await;
                                    }
                                }
                            } else {
                                let _ = tx
                                    .send(StreamEvent::Error(ErrorData {
                                        code: code.clone(),
                                        message,
                                    }))
                                    .await;
                            }

                            // Metrics: mid-stream failure
                            if let Some(ref fctx) = fin_ctx {
                                let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                                fctx.metrics.record_stream_failed(&fctx.provider_id, &fctx.effective_model, &code);
                                fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                            }

                            provider_stream.cancel();
                            let has_partial = !accumulated_text.is_empty();
                            return StreamOutcome {
                                terminal: StreamTerminal::Failed,
                                accumulated_text,
                                usage: None,
                                effective_model: model,
                                error_code: Some(code),
                                provider_response_id: None,
                                provider_partial_usage: has_partial,
                            };
                        }
                        None => {
                            // Stream ended — terminal captured by ProviderStream
                            break;
                        }
                    }
                }
            }
        }

        if cancelled {
            let elapsed = stream_start.elapsed();
            info!(
                terminal = "cancelled",
                duration_ms = elapsed.as_millis() as u64,
                "stream cancelled"
            );

            // Finalize cancelled turn — no SSE emission (stream already disconnected) (D3)
            if let Some(ref fctx) = fin_ctx {
                let input = fctx.to_finalization_input(
                    TurnState::Cancelled,
                    &accumulated_text,
                    None,
                    None,
                    None,
                    None,
                    web_search_completed_count,
                    code_interpreter_completed_count,
                    knowledge_call_count,
                    first_token_time.map(|d| d.as_millis() as u64),
                    Some(elapsed.as_millis() as u64),
                );
                if let Err(e) = fctx.finalization_svc.finalize_turn_cas(input).await {
                    warn!(error = %e, "finalization failed on cancelled stream");
                }

                // Metrics: cancelled stream
                let ms = elapsed.as_secs_f64() * 1000.0;
                fctx.metrics.record_cancel_effective(trigger::DISCONNECT);
                fctx.metrics.record_time_to_abort_ms(trigger::DISCONNECT, ms);
                fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
            }

            return StreamOutcome {
                terminal: StreamTerminal::Cancelled,
                accumulated_text,
                usage: None,
                effective_model: model,
                error_code: None,
                provider_response_id: None,
                provider_partial_usage: false,
            };
        }

        // Extract the terminal outcome from the provider stream
        let terminal = provider_stream.into_outcome().await;

        match terminal {
            TerminalOutcome::Completed {
                usage,
                content: _,
                citations,
                response_id,
                ..
            } => {
                let elapsed = stream_start.elapsed();
                info!(
                    terminal = "completed",
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    duration_ms = elapsed.as_millis() as u64,
                    "stream completed"
                );

                // Finalize first, then emit Done only if CAS winner (D3)
                if let Some(ref fctx) = fin_ctx {
                    let input = fctx.to_finalization_input(
                        TurnState::Completed,
                        &accumulated_text,
                        Some(usage),
                        None,
                        None,
                        Some(response_id.clone()),
                        web_search_completed_count,
                        code_interpreter_completed_count,
                        knowledge_call_count,
                        first_token_time.map(|d| d.as_millis() as u64),
                        Some(elapsed.as_millis() as u64),
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            // P4-2: Map provider file_ids to internal UUIDs
                            let mapped = crate::domain::citation_mapping::map_citation_ids(
                                citations,
                                &provider_file_id_map,
                            );
                            if !mapped.is_empty() {
                                let _ = tx
                                    .send(StreamEvent::Citations(
                                        crate::domain::stream_events::CitationsData {
                                            items: mapped,
                                        },
                                    ))
                                    .await;
                            }
                            // Compute quota warnings post-commit (advisory, best-effort)
                            let quota_warnings = match fctx
                                .quota_warnings_provider
                                .get_quota_warnings(&fctx.scope, fctx.tenant_id, fctx.user_id)
                                .await
                            {
                                Ok(w) => Some(w),
                                Err(e) => {
                                    warn!(error = %e, "failed to compute quota_warnings");
                                    None
                                }
                            };
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: fctx.quota_decision.clone(),
                                    downgrade_from: fctx.downgrade_from.clone(),
                                    downgrade_reason: fctx.downgrade_reason.clone(),
                                    quota_warnings,
                                })))
                                .await;
                        }
                        Ok(_) => { /* CAS loser — no SSE emission */ }
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on completed stream");
                            // Emit Done anyway so client isn't left hanging
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: fctx.quota_decision.clone(),
                                    downgrade_from: fctx.downgrade_from.clone(),
                                    downgrade_reason: fctx.downgrade_reason.clone(),
                                    quota_warnings: None,
                                })))
                                .await;
                        }
                    }
                } else {
                    // No finalization context (unit tests) — emit directly
                    let mapped = crate::domain::citation_mapping::map_citation_ids(
                        citations,
                        &provider_file_id_map,
                    );
                    if !mapped.is_empty() {
                        let _ = tx
                            .send(StreamEvent::Citations(
                                crate::domain::stream_events::CitationsData { items: mapped },
                            ))
                            .await;
                    }
                    let _ = tx
                        .send(StreamEvent::Done(Box::new(DoneData {
                            usage: Some(usage),
                            effective_model: model.clone(),
                            selected_model: model.clone(),
                            quota_decision: "allow".into(),
                            downgrade_from: None,
                            downgrade_reason: None,
                            quota_warnings: None,
                        })))
                        .await;
                }

                // Metrics: completed stream
                if let Some(ref fctx) = fin_ctx {
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_completed(&fctx.provider_id, &fctx.effective_model);
                    fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                }

                return StreamOutcome {
                    terminal: StreamTerminal::Completed,
                    accumulated_text,
                    usage: Some(usage),
                    effective_model: model,
                    error_code: None,
                    provider_response_id: Some(response_id),
                    provider_partial_usage: false,
                };
            }
            TerminalOutcome::Incomplete { usage, reason, .. } => {
                let elapsed = stream_start.elapsed();
                warn!(
                    terminal = "incomplete",
                    reason = %reason,
                    duration_ms = elapsed.as_millis() as u64,
                    "stream incomplete"
                );

                // Incomplete maps to Completed in DB — provider finished but hit
                // max_output_tokens. From billing/persistence perspective this is
                // a completed turn with truncated content (see design D10).
                if let Some(ref fctx) = fin_ctx {
                    let input = fctx.to_finalization_input(
                        TurnState::Completed,
                        &accumulated_text,
                        Some(usage),
                        None,
                        None,
                        None,
                        web_search_completed_count,
                        code_interpreter_completed_count,
                        knowledge_call_count,
                        first_token_time.map(|d| d.as_millis() as u64),
                        Some(elapsed.as_millis() as u64),
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            let quota_warnings = match fctx
                                .quota_warnings_provider
                                .get_quota_warnings(&fctx.scope, fctx.tenant_id, fctx.user_id)
                                .await
                            {
                                Ok(w) => Some(w),
                                Err(e) => {
                                    warn!(error = %e, "failed to compute quota_warnings");
                                    None
                                }
                            };
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: fctx.quota_decision.clone(),
                                    downgrade_from: fctx.downgrade_from.clone(),
                                    downgrade_reason: fctx.downgrade_reason.clone(),
                                    quota_warnings,
                                })))
                                .await;
                        }
                        Ok(_) => {}
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on incomplete stream");
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: fctx.quota_decision.clone(),
                                    downgrade_from: fctx.downgrade_from.clone(),
                                    downgrade_reason: fctx.downgrade_reason.clone(),
                                    quota_warnings: None,
                                })))
                                .await;
                        }
                    }
                } else {
                    let _ = tx
                        .send(StreamEvent::Done(Box::new(DoneData {
                            usage: Some(usage),
                            effective_model: model.clone(),
                            selected_model: model.clone(),
                            quota_decision: "allow".into(),
                            downgrade_from: None,
                            downgrade_reason: None,
                            quota_warnings: None,
                        })))
                        .await;
                }

                // Metrics: incomplete stream
                if let Some(ref fctx) = fin_ctx {
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_incomplete(&fctx.provider_id, &fctx.effective_model, &reason);
                    fctx.metrics.record_stream_completed(&fctx.provider_id, &fctx.effective_model);
                    fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                }

                return StreamOutcome {
                    terminal: StreamTerminal::Incomplete,
                    accumulated_text,
                    usage: Some(usage),
                    effective_model: model,
                    error_code: Some(format!("incomplete:{reason}")),
                    provider_response_id: None,
                    provider_partial_usage: false,
                };
            }
            TerminalOutcome::Failed { error, usage, .. } => {
                let raw_detail = error.raw_detail().map(ToOwned::to_owned);
                let (code, message) = normalize_error(&error);
                let elapsed = stream_start.elapsed();
                warn!(
                    terminal = "failed",
                    error_code = %code,
                    raw_detail = raw_detail.as_deref().unwrap_or(""),
                    duration_ms = elapsed.as_millis() as u64,
                    "stream failed"
                );

                // Finalize first, emit error only if CAS winner (D3)
                if let Some(ref fctx) = fin_ctx {
                    let input = fctx.to_finalization_input(
                        TurnState::Failed,
                        &accumulated_text,
                        usage,
                        Some(code.clone()),
                        None,
                        None,
                        web_search_completed_count,
                        code_interpreter_completed_count,
                        knowledge_call_count,
                        first_token_time.map(|d| d.as_millis() as u64),
                        Some(elapsed.as_millis() as u64),
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                        Ok(_) => {}
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on failed stream");
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                    }
                } else {
                    let _ = tx
                        .send(StreamEvent::Error(ErrorData {
                            code: code.clone(),
                            message,
                        }))
                        .await;
                }

                // Metrics: failed stream (post-provider)
                if let Some(ref fctx) = fin_ctx {
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_failed(&fctx.provider_id, &fctx.effective_model, &code);
                    fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                }

                return StreamOutcome {
                    terminal: StreamTerminal::Failed,
                    accumulated_text,
                    usage,
                    effective_model: model,
                    error_code: Some(code),
                    provider_response_id: None,
                    provider_partial_usage: usage.is_some(),
                };
            }
            TerminalOutcome::ToolUse {
                tool_use_id,
                name,
                input,
            } => {
                if name == "search_knowledge"
                    && let Some(ref ks) = knowledge_search
                {
                        // Enforce per-message call limit — graceful degradation.
                        // Instead of failing the turn, inject a soft limit notice as a
                        // function_call_output so the model can still answer from whatever
                        // it has already retrieved.
                        if knowledge_call_count >= ks.max_calls {
                            warn!(
                                knowledge_call_count,
                                limit = ks.max_calls,
                                "knowledge search per-message limit reached, injecting soft limit response"
                            );
                            let raw_arguments =
                                serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_owned());
                            raw_input_items.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": tool_use_id,
                                "name": "search_knowledge",
                                "arguments": raw_arguments,
                            }));
                            raw_input_items.push(serde_json::json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": "Search limit reached for this message. \
                                           Please answer based on the information already retrieved.",
                            }));
                            continue 'agentic;
                        }

                        knowledge_call_count += 1;

                        // Extract arguments. top_k from the model is capped at
                        // ks.top_k so the model cannot inflate retrieval cost.
                        let query = input
                            .get("query")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_owned();
                        let top_k = input
                            .get("top_k")
                            .and_then(|v| v.as_u64())
                            .map(|v| (v as usize).min(ks.top_k))
                            .unwrap_or(ks.top_k);
                        let raw_arguments =
                            serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_owned());

                        // Append the model's function_call item to replay history.
                        raw_input_items.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": tool_use_id,
                            "name": "search_knowledge",
                            "arguments": raw_arguments,
                        }));

                        // Call the retriever.
                        let retrieval_start = std::time::Instant::now();
                        let retrieval_result = ks
                            .retriever
                            .retrieve(
                                ctx.clone(),
                                RetrievalRequest {
                                    query,
                                    top_k,
                                    chat_id: fin_ctx
                                        .as_ref()
                                        .map_or_else(String::new, |f| f.chat_id.to_string()),
                                    vector_store_id: ks.vector_store_id.clone(),
                                    upstream_alias: ks.upstream_alias.clone(),
                                    api_version: ks.api_version.clone(),
                                },
                            )
                            .await;
                        let retrieval_ms =
                            retrieval_start.elapsed().as_secs_f64() * 1000.0;

                        let output_text = match retrieval_result {
                            Ok(raw_chunks) => {
                                let chunks =
                                    post_process_chunks(raw_chunks, ks.max_chunk_chars);
                                if let Some(ref fctx) = fin_ctx {
                                    fctx.metrics.record_knowledge_search("ok");
                                    fctx.metrics
                                        .record_knowledge_search_latency_ms(retrieval_ms);
                                    fctx.metrics
                                        .record_knowledge_search_chunks(chunks.len() as f64);

                                    // Persist increment to chat_turns so the
                                    // orphan watchdog can recover the count if
                                    // the pod dies before stream finalization.
                                    // Same pattern as web_search / code_interpreter.
                                    if let Ok(conn) = fctx.db.conn() {
                                        if let Err(e) = fctx.turn_repo.increment_tool_calls(
                                            &conn,
                                            &fctx.scope,
                                            fctx.turn_id,
                                            ToolCallType::FileSearch,
                                        ).await {
                                            warn!(
                                                turn_id = %fctx.turn_id,
                                                error = %e,
                                                "failed to persist file_search_completed_count"
                                            );
                                        }
                                    } else {
                                        warn!(
                                            turn_id = %fctx.turn_id,
                                            "failed to acquire DB conn for file_search_completed_count"
                                        );
                                    }
                                }
                                if ks.use_search_result_blocks {
                                    format_chunks_as_search_result_json(&chunks)
                                } else {
                                    format_chunks_as_text(&chunks)
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "knowledge retrieval failed");
                                if let Some(ref fctx) = fin_ctx {
                                    fctx.metrics.record_knowledge_search("error");
                                    fctx.metrics
                                        .record_knowledge_search_latency_ms(retrieval_ms);
                                }
                                // Distinct from the legitimate "empty result"
                                // message so the model can tell a retriever
                                // failure from a zero-hit query and adjust.
                                "Knowledge search failed; answer without retrieved context."
                                    .to_owned()
                            }
                        };

                        // Append the function_call_output item to replay history.
                        raw_input_items.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": output_text,
                        }));

                        continue 'agentic;
                }

                // Generic custom function tools (e.g. exa_search). Dispatched by
                // name through the per-request registry. Mirrors the
                // search_knowledge path: replay the function_call, execute, inject
                // the function_call_output, and continue the agentic loop.
                if let Some(tool) = function_tools.get(&name) {
                    let raw_arguments =
                        serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_owned());
                    let count = function_tool_calls.entry(name.clone()).or_insert(0);

                    // Soft per-tool limit — inject a notice instead of failing.
                    if *count >= tool.max_calls() {
                        warn!(
                            tool = %name,
                            limit = tool.max_calls(),
                            "function tool per-message limit reached, injecting soft limit response"
                        );
                        raw_input_items.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": tool_use_id,
                            "name": name,
                            "arguments": raw_arguments,
                        }));
                        raw_input_items.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": "Tool call limit reached for this message. \
                                       Please answer based on the information already gathered.",
                        }));
                        continue 'agentic;
                    }
                    *count += 1;

                    // Append the model's function_call item to replay history.
                    raw_input_items.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": tool_use_id,
                        "name": name,
                        "arguments": raw_arguments,
                    }));

                    let exec_start = std::time::Instant::now();
                    let output_text = match tool.execute(ctx.clone(), input).await {
                        Ok(text) => text,
                        Err(e) => {
                            // Distinct from a legitimate empty result so the model
                            // can tell a tool failure from a zero-hit query.
                            warn!(tool = %name, error = %e, "function tool execution failed");
                            "Tool call failed; answer without the tool result.".to_owned()
                        }
                    };
                    debug!(
                        tool = %name,
                        elapsed_ms = exec_start.elapsed().as_millis() as u64,
                        "function tool executed"
                    );

                    raw_input_items.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": output_text,
                    }));

                    continue 'agentic;
                }

                // Unrecognised tool while custom function tools ARE enabled.
                // Models like gpt-oss emit their built-in browser actions
                // (`open`, `find`, `search`) that we don't provide. Don't fail
                // the turn — replay the call with an "unavailable" output so the
                // model answers from what it already gathered (e.g. exa_search
                // results). The agentic-iteration cap bounds a model that keeps
                // ignoring the notice.
                if !function_tools.is_empty() {
                    warn!(
                        tool = %name,
                        "unsupported tool requested; injecting unavailable notice and continuing"
                    );
                    let raw_arguments =
                        serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_owned());
                    let notice = format!(
                        "The tool \"{name}\" is not available. Do not call it again. \
                         Answer the user directly using the information already gathered."
                    );
                    raw_input_items.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": tool_use_id,
                        "name": name,
                        "arguments": raw_arguments,
                    }));
                    raw_input_items.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": notice,
                    }));
                    continue 'agentic;
                }

                // No custom tools enabled — genuinely unsupported. Fail the turn.
                warn!(tool = %name, "unexpected ToolUse outcome; finalizing as failed");
                let code = "unexpected_tool_use".to_owned();
                let message = "Provider requested an unsupported function tool".to_owned();
                if let Some(ref fctx) = fin_ctx {
                    let elapsed = stream_start.elapsed();
                    let finput = fctx.to_finalization_input(
                        TurnState::Failed,
                        &accumulated_text,
                        None,
                        Some(code.clone()),
                        None,
                        None,
                        web_search_completed_count,
                        code_interpreter_completed_count,
                        knowledge_call_count,
                        first_token_time.map(|d| d.as_millis() as u64),
                        Some(elapsed.as_millis() as u64),
                    );
                    match fctx.finalization_svc.finalize_turn_cas(finput).await {
                        Ok(outcome) if outcome.won_cas => {
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                        Ok(_) => {}
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on unexpected tool use");
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                    }
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics
                        .record_stream_failed(&fctx.provider_id, &fctx.effective_model, &code);
                    fctx.metrics.record_stream_total_latency_ms(
                        &fctx.provider_id,
                        &fctx.effective_model,
                        ms,
                    );
                } else {
                    let _ = tx
                        .send(StreamEvent::Error(ErrorData {
                            code: code.clone(),
                            message,
                        }))
                        .await;
                }
                let has_partial = !accumulated_text.is_empty();
                return StreamOutcome {
                    terminal: StreamTerminal::Failed,
                    accumulated_text,
                    usage: None,
                    effective_model: model,
                    error_code: Some(code),
                    provider_response_id: None,
                    provider_partial_usage: has_partial,
                };
            }
        }

        } // end 'agentic loop
    }.instrument(span))
}

/// Post-process raw retrieval results before injecting them into the model context.
///
/// Steps applied in order:
/// 1. **Sort** by relevance score descending (highest score first).
/// 2. **Deduplicate** — remove chunks whose text is identical to an earlier chunk.
///    Prevents wasting tokens on overlapping windows from the same document.
/// 3. **Assign stable chunk indices** — appends `#chunk/{i}` to each `source_uri`
///    so citations are traceable back to a specific chunk position.
/// 4. **Truncate** each chunk's text to `max_chars` to bound context token cost.
fn post_process_chunks(mut chunks: Vec<RetrievedChunk>, max_chars: usize) -> Vec<RetrievedChunk> {
    // 1. Sort by score descending.
    chunks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // 2. Deduplicate by exact text content.
    let mut seen = std::collections::HashSet::new();
    chunks.retain(|c| seen.insert(c.text.clone()));

    // 3. Assign stable chunk index to source_uri.
    for (i, chunk) in chunks.iter_mut().enumerate() {
        // Strip any existing fragment before appending so re-runs are idempotent.
        if let Some(base) = chunk.source_uri.split_once('#') {
            chunk.source_uri = format!("{}#chunk/{i}", base.0);
        } else {
            chunk.source_uri = format!("{}#chunk/{i}", chunk.source_uri);
        }
    }

    // 4. Truncate text at a valid UTF-8 char boundary.
    for chunk in &mut chunks {
        if chunk.text.len() > max_chars {
            let mut boundary = max_chars;
            while !chunk.text.is_char_boundary(boundary) {
                boundary -= 1;
            }
            chunk.text.truncate(boundary);
        }
    }

    chunks
}

/// Format retrieved knowledge chunks as Anthropic `search_result` JSON blocks.
///
/// Produces a JSON array of `search_result` objects. When this string is set as
/// `function_call_output.output`, the Anthropic adapter's `parse_tool_result_content`
/// recognises the typed-block array and forwards it verbatim as `tool_result` content,
/// enabling Anthropic's native citation machinery.
fn format_chunks_as_search_result_json(chunks: &[RetrievedChunk]) -> String {
    if chunks.is_empty() {
        return serde_json::json!([{
            "type": "text",
            "text": "No relevant content found."
        }])
        .to_string();
    }
    let blocks: Vec<serde_json::Value> = chunks
        .iter()
        .map(|chunk| {
            serde_json::json!({
                "type": "search_result",
                "source": chunk.source_uri,
                "title": chunk.title,
                "content": [{"type": "text", "text": chunk.text}]
            })
        })
        .collect();
    serde_json::Value::Array(blocks).to_string()
}

/// Format retrieved knowledge chunks as a text block for the LLM.
///
/// Uses `[SOURCE_N]` labels so the model can inline-cite them naturally.
/// The Responses API does not support Anthropic-style `search_result` content
/// blocks, so plain text with explicit source labels is the correct approach
/// for OpenAI/Azure providers.
fn format_chunks_as_text(chunks: &[RetrievedChunk]) -> String {
    use std::fmt::Write as _;
    if chunks.is_empty() {
        return "No relevant content found.".to_owned();
    }
    chunks
        .iter()
        .enumerate()
        .fold(String::new(), |mut out, (i, chunk)| {
            write!(
                out,
                "[SOURCE_{}] \"{}\"\n{}\n\n",
                i + 1,
                chunk.title,
                chunk.text,
            )
            .ok();
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(source_uri: &str, title: &str, text: &str) -> RetrievedChunk {
        RetrievedChunk {
            source_uri: source_uri.to_owned(),
            title: title.to_owned(),
            text: text.to_owned(),
            score: 1.0,
        }
    }

    #[test]
    fn format_search_result_empty_returns_text_block_with_no_content_message() {
        let json = format_chunks_as_search_result_json(&[]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["type"], "text");
        assert_eq!(v[0]["text"], "No relevant content found.");
    }

    #[test]
    fn format_search_result_single_chunk_has_correct_fields() {
        let chunks = [chunk("kb://doc/1#chunk/0", "Doc 1", "Some text")];
        let json = format_chunks_as_search_result_json(&chunks);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["type"], "search_result");
        assert_eq!(v[0]["source"], "kb://doc/1#chunk/0");
        assert_eq!(v[0]["title"], "Doc 1");
        assert_eq!(v[0]["content"][0]["type"], "text");
        assert_eq!(v[0]["content"][0]["text"], "Some text");
    }

    #[test]
    fn format_search_result_multiple_chunks_all_present() {
        let chunks = [
            chunk("kb://doc/1#chunk/0", "Doc 1", "Text one"),
            chunk("kb://doc/2#chunk/1", "Doc 2", "Text two"),
        ];
        let json = format_chunks_as_search_result_json(&chunks);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["type"], "search_result");
        assert_eq!(v[1]["source"], "kb://doc/2#chunk/1");
        assert_eq!(v[1]["title"], "Doc 2");
    }

    #[test]
    fn format_search_result_all_blocks_have_type_field_for_passthrough() {
        // parse_tool_result_content in anthropic_messages forwards a JSON array
        // verbatim only when every element has a "type" field.
        let chunks = [
            chunk("kb://doc/1#chunk/0", "Doc 1", "Hello"),
            chunk("kb://doc/2#chunk/1", "Doc 2", "World"),
        ];
        let json = format_chunks_as_search_result_json(&chunks);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = v.as_array().unwrap();
        assert!(arr.iter().all(|block| block.get("type").is_some()));
    }

    // ── format_chunks_as_text ──

    fn chunk_with_score(source_uri: &str, title: &str, text: &str, score: f32) -> RetrievedChunk {
        RetrievedChunk {
            source_uri: source_uri.to_owned(),
            title: title.to_owned(),
            text: text.to_owned(),
            score,
        }
    }

    #[test]
    fn format_text_empty_returns_no_content_message() {
        let out = format_chunks_as_text(&[]);
        assert_eq!(out, "No relevant content found.");
    }

    #[test]
    fn format_text_single_chunk_uses_source_1_label() {
        let chunks = [chunk("kb://doc/1", "Title", "Body text")];
        let out = format_chunks_as_text(&chunks);
        assert!(out.contains("[SOURCE_1]"));
        assert!(out.contains("\"Title\""));
        assert!(out.contains("Body text"));
    }

    #[test]
    fn format_text_multiple_chunks_numbered_sequentially() {
        let chunks = [
            chunk("kb://doc/1", "First", "aaa"),
            chunk("kb://doc/2", "Second", "bbb"),
            chunk("kb://doc/3", "Third", "ccc"),
        ];
        let out = format_chunks_as_text(&chunks);
        assert!(out.contains("[SOURCE_1]"));
        assert!(out.contains("[SOURCE_2]"));
        assert!(out.contains("[SOURCE_3]"));
        // Labels must appear in order.
        let p1 = out.find("[SOURCE_1]").unwrap();
        let p2 = out.find("[SOURCE_2]").unwrap();
        let p3 = out.find("[SOURCE_3]").unwrap();
        assert!(p1 < p2 && p2 < p3);
    }

    // ── post_process_chunks ──

    #[test]
    fn post_process_sorts_by_score_descending() {
        let chunks = vec![
            chunk_with_score("kb://a", "A", "text-a", 0.1),
            chunk_with_score("kb://b", "B", "text-b", 0.9),
            chunk_with_score("kb://c", "C", "text-c", 0.5),
        ];
        let out = post_process_chunks(chunks, 1000);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].text, "text-b");
        assert_eq!(out[1].text, "text-c");
        assert_eq!(out[2].text, "text-a");
    }

    #[test]
    fn post_process_deduplicates_identical_text() {
        let chunks = vec![
            chunk_with_score("kb://a", "A", "same", 0.9),
            chunk_with_score("kb://b", "B", "same", 0.8),
            chunk_with_score("kb://c", "C", "different", 0.5),
        ];
        let out = post_process_chunks(chunks, 1000);
        assert_eq!(out.len(), 2);
        // Sort happens first → dedup keeps the highest-scoring duplicate.
        assert_eq!(out[0].text, "same");
        assert_eq!(out[0].source_uri, "kb://a#chunk/0");
        assert_eq!(out[1].text, "different");
    }

    #[test]
    fn post_process_assigns_stable_chunk_index_to_source_uri() {
        let chunks = vec![
            chunk_with_score("kb://doc/x", "X", "first", 0.9),
            chunk_with_score("kb://doc/y", "Y", "second", 0.5),
        ];
        let out = post_process_chunks(chunks, 1000);
        assert_eq!(out[0].source_uri, "kb://doc/x#chunk/0");
        assert_eq!(out[1].source_uri, "kb://doc/y#chunk/1");
    }

    #[test]
    fn post_process_replaces_existing_fragment_when_reassigning_index() {
        // Idempotence: if post_process_chunks runs twice, the existing
        // `#chunk/{i}` fragment is stripped and re-assigned instead of
        // doubling up (e.g., `#chunk/0#chunk/0`).
        let chunks = vec![chunk_with_score("kb://doc/x#chunk/9", "X", "once", 0.9)];
        let out = post_process_chunks(chunks, 1000);
        assert_eq!(out[0].source_uri, "kb://doc/x#chunk/0");
    }

    #[test]
    fn post_process_truncates_text_to_max_chars() {
        let chunks = vec![chunk_with_score("kb://doc/x", "X", "abcdefghij", 0.9)];
        let out = post_process_chunks(chunks, 5);
        assert_eq!(out[0].text, "abcde");
    }

    #[test]
    fn post_process_truncates_at_utf8_char_boundary() {
        // "héllo" — 'é' is two bytes (0xc3 0xa9) at positions 1..3.
        // A naive truncate(2) would split inside 'é'. post_process_chunks
        // must find a char boundary and truncate to a valid UTF-8 string.
        let chunks = vec![chunk_with_score("kb://doc/x", "X", "héllo", 0.9)];
        let out = post_process_chunks(chunks, 2);
        // Must not panic and must be valid UTF-8. Boundary-safe truncation
        // yields either 1 byte ("h") or stays at 2 if it lands on a boundary;
        // the actual result here is "h" since byte 2 is inside 'é'.
        assert!(out[0].text.is_char_boundary(out[0].text.len()));
        assert_eq!(out[0].text, "h");
    }

    #[test]
    fn post_process_empty_input_returns_empty() {
        let out = post_process_chunks(vec![], 1000);
        assert!(out.is_empty());
    }
}
