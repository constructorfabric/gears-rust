use std::sync::Arc;

use super::current_otel_trace_id;
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use modkit_macros::domain_model;
use modkit_security::{AccessScope, SecurityContext};
use tracing::info;
use uuid::Uuid;

use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::ports::metric_labels::{op, result as result_label};
use mini_chat_sdk::{
    RequesterType, TurnDeleteAuditEvent, TurnDeleteAuditEventType, TurnMutationAuditEvent,
};

use crate::domain::repos::{
    ChatRepository, CreateTurnParams, InsertUserMessageParams, MessageAttachmentRepository,
    MessageRepository, OutboxEnqueuer, TurnRepository,
};
use crate::domain::service::AuditEnvelope;
use crate::infra::db::entity::chat_turn::{Model as TurnModel, TurnState};

use super::{DbProvider, actions, resources};

// ════════════════════════════════════════════════════════════════════════════
// MutationError
// ════════════════════════════════════════════════════════════════════════════

/// Error type for turn mutation operations (retry, edit, delete).
/// Each variant maps to a specific HTTP status and error code.
#[domain_model]
#[derive(Debug)]
pub enum MutationError {
    ChatNotFound { chat_id: Uuid },
    TurnNotFound { chat_id: Uuid, request_id: Uuid },
    Forbidden,
    InvalidTurnState { state: TurnState },
    NotLatestTurn,
    GenerationInProgress,
    Internal { message: String },
}

impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChatNotFound { chat_id } => write!(f, "Chat not found: {chat_id}"),
            Self::TurnNotFound {
                chat_id,
                request_id,
            } => {
                write!(f, "Turn {request_id} not found in chat {chat_id}")
            }
            Self::Forbidden => write!(f, "Access denied"),
            Self::InvalidTurnState { state } => {
                let label = match state {
                    TurnState::Running => "running",
                    TurnState::Completed => "completed",
                    TurnState::Failed => "failed",
                    TurnState::Cancelled => "cancelled",
                };
                write!(f, "Invalid turn state: {label}")
            }
            Self::NotLatestTurn => write!(f, "Target is not the latest turn"),
            Self::GenerationInProgress => {
                write!(f, "A generation is already in progress")
            }
            Self::Internal { message } => write!(f, "Internal error: {message}"),
        }
    }
}

impl std::error::Error for MutationError {}

impl From<EnforcerError> for MutationError {
    #[allow(clippy::cognitive_complexity)]
    fn from(e: EnforcerError) -> Self {
        match e {
            EnforcerError::Denied { ref deny_reason } => {
                tracing::warn!(deny_reason = ?deny_reason, "AuthZ denied access");
                Self::Forbidden
            }
            EnforcerError::CompileFailed(ref err) => {
                tracing::warn!(error = %err, "AuthZ constraint compile failed - access denied");
                Self::Forbidden
            }
            EnforcerError::EvaluationFailed(ref err) => {
                tracing::error!(error = %err, "AuthZ evaluation failed (internal error)");
                Self::Internal {
                    message: err.to_string(),
                }
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Results
// ════════════════════════════════════════════════════════════════════════════

/// Returned from retry/edit. Contains everything the handler needs to
/// set up streaming via `StreamService::run_stream_for_mutation()`.
#[domain_model]
#[derive(Debug)]
pub struct MutationResult {
    pub new_request_id: Uuid,
    pub new_turn_id: Uuid,
    pub user_content: String,
    /// Snapshot boundary computed before the new user message was persisted.
    /// Ensures deterministic context assembly (DESIGN `§ContextPlan` Determinism P1).
    pub snapshot_boundary: Option<crate::domain::repos::SnapshotBoundary>,
    /// Chat model carried from the mutation transaction so the handler can
    /// resolve the provider without a redundant DB round-trip.
    pub chat_model: String,
}

// ════════════════════════════════════════════════════════════════════════════
// TurnService
// ════════════════════════════════════════════════════════════════════════════

#[domain_model]
pub struct TurnService<
    TR: TurnRepository + 'static,
    MR: MessageRepository + 'static,
    CR: ChatRepository + 'static,
    MAR: MessageAttachmentRepository + 'static,
> {
    pub(crate) db: Arc<DbProvider>,
    pub(crate) turn_repo: Arc<TR>,
    pub(crate) message_repo: Arc<MR>,
    chat_repo: Arc<CR>,
    message_attachment_repo: Arc<MAR>,
    enforcer: PolicyEnforcer,
    outbox_enqueuer: Arc<dyn OutboxEnqueuer>,
    metrics: Arc<dyn MiniChatMetricsPort>,
}

impl<
    TR: TurnRepository + 'static,
    MR: MessageRepository + 'static,
    CR: ChatRepository + 'static,
    MAR: MessageAttachmentRepository + 'static,
> TurnService<TR, MR, CR, MAR>
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        db: Arc<DbProvider>,
        turn_repo: Arc<TR>,
        message_repo: Arc<MR>,
        chat_repo: Arc<CR>,
        message_attachment_repo: Arc<MAR>,
        enforcer: PolicyEnforcer,
        outbox_enqueuer: Arc<dyn OutboxEnqueuer>,
        metrics: Arc<dyn MiniChatMetricsPort>,
    ) -> Self {
        Self {
            db,
            turn_repo,
            message_repo,
            chat_repo,
            message_attachment_repo,
            enforcer,
            outbox_enqueuer,
            metrics,
        }
    }

    // ── Get ─────────────────────────────────────────────────────────────

    pub async fn get(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        request_id: Uuid,
    ) -> Result<TurnModel, MutationError> {
        let chat_scope = self
            .enforcer
            .access_scope(ctx, &resources::CHAT, actions::READ_TURN, Some(chat_id))
            .await?
            .ensure_owner(ctx.subject_id());

        let conn = self.db.conn().map_err(|e| MutationError::Internal {
            message: e.to_string(),
        })?;

        // Verify chat exists (scoped by authz)
        self.chat_repo
            .get(&conn, &chat_scope, chat_id)
            .await
            .map_err(|e| MutationError::Internal {
                message: e.to_string(),
            })?
            .ok_or(MutationError::ChatNotFound { chat_id })?;

        let scope = chat_scope.tenant_only();

        self.turn_repo
            .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
            .await
            .map_err(|e| MutationError::Internal {
                message: e.to_string(),
            })?
            .ok_or(MutationError::TurnNotFound {
                chat_id,
                request_id,
            })
    }

    // ── Delete ──────────────────────────────────────────────────────────

    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        request_id: Uuid,
    ) -> Result<(), MutationError> {
        info!(%chat_id, %request_id, "turn delete");

        let chat_scope = self
            .enforcer
            .access_scope(ctx, &resources::CHAT, actions::DELETE_TURN, Some(chat_id))
            .await?
            .ensure_owner(ctx.subject_id());

        let start = std::time::Instant::now();
        // Capture trace_id before the transaction; the closure runs in a different
        // async context and does not inherit the parent span.
        let trace_id = current_otel_trace_id();

        let turn_repo = Arc::clone(&self.turn_repo);
        let message_repo = Arc::clone(&self.message_repo);
        let chat_repo = Arc::clone(&self.chat_repo);
        let outbox_enqueuer = Arc::clone(&self.outbox_enqueuer);
        let scope_tx = chat_scope.clone();
        let ctx_clone = ctx.clone();

        let result = self
            .db
            .transaction(|tx| {
                Box::pin(async move {
                    let (scope, target, _chat_model) = validate_mutation(
                        &*chat_repo,
                        &*turn_repo,
                        &scope_tx,
                        &ctx_clone,
                        tx,
                        chat_id,
                        request_id,
                    )
                    .await
                    .map_err(mutation_to_db_err)?;

                    turn_repo
                        .soft_delete(tx, &scope, target.id, None)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;
                    message_repo
                        .soft_delete_by_request_id(tx, &scope, chat_id, request_id)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    // Enqueue audit event atomically within the same transaction.
                    let audit_event = AuditEnvelope::Delete(TurnDeleteAuditEvent {
                        event_type: TurnDeleteAuditEventType::default(),
                        timestamp: time::OffsetDateTime::now_utc(),
                        tenant_id: ctx_clone.subject_tenant_id(),
                        requester_type: requester_type_from_subject(&ctx_clone),
                        trace_id,
                        actor_user_id: ctx_clone.subject_id(),
                        chat_id,
                        turn_id: target.id,
                        request_id,
                    });
                    outbox_enqueuer
                        .enqueue_audit_event(tx, audit_event)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    Ok(())
                })
            })
            .await
            .map_err(unwrap_mutation_err);

        let ms = start.elapsed().as_secs_f64() * 1000.0;
        self.metrics
            .record_turn_mutation(op::DELETE, mutation_result_label(&result));
        self.metrics.record_turn_mutation_latency_ms(op::DELETE, ms);
        result?;

        // Post-commit side effects (outside transaction).
        self.outbox_enqueuer.flush();

        Ok(())
    }

    // ── Retry ───────────────────────────────────────────────────────────

    pub async fn retry(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        request_id: Uuid,
    ) -> Result<MutationResult, MutationError> {
        info!(%chat_id, %request_id, "turn retry");

        let chat_scope = self
            .enforcer
            .access_scope(ctx, &resources::CHAT, actions::RETRY_TURN, Some(chat_id))
            .await?
            .ensure_owner(ctx.subject_id());

        let start = std::time::Instant::now();
        // Capture trace_id before the transaction closure.
        let trace_id = current_otel_trace_id();
        let result = self
            .mutate_for_stream(ctx, chat_scope, chat_id, request_id, None, trace_id)
            .await;

        let ms = start.elapsed().as_secs_f64() * 1000.0;
        self.metrics
            .record_turn_mutation(op::RETRY, mutation_result_label(&result));
        self.metrics.record_turn_mutation_latency_ms(op::RETRY, ms);
        let result = result?;

        // Post-commit side effects (outside transaction).
        self.outbox_enqueuer.flush();

        Ok(result)
    }

    // ── Edit ────────────────────────────────────────────────────────────

    pub async fn edit(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        request_id: Uuid,
        new_content: String,
    ) -> Result<MutationResult, MutationError> {
        info!(%chat_id, %request_id, "turn edit");

        let chat_scope = self
            .enforcer
            .access_scope(ctx, &resources::CHAT, actions::EDIT_TURN, Some(chat_id))
            .await?
            .ensure_owner(ctx.subject_id());

        let start = std::time::Instant::now();
        // Capture trace_id before the transaction closure.
        let trace_id = current_otel_trace_id();
        let result = self
            .mutate_for_stream(
                ctx,
                chat_scope,
                chat_id,
                request_id,
                Some(new_content),
                trace_id,
            )
            .await;

        let ms = start.elapsed().as_secs_f64() * 1000.0;
        self.metrics
            .record_turn_mutation(op::EDIT, mutation_result_label(&result));
        self.metrics.record_turn_mutation_latency_ms(op::EDIT, ms);
        let result = result?;

        // Post-commit side effects (outside transaction).
        self.outbox_enqueuer.flush();

        Ok(result)
    }

    // ── Shared retry/edit transaction ────────────────────────────────────

    async fn mutate_for_stream(
        &self,
        ctx: &SecurityContext,
        chat_scope: AccessScope,
        chat_id: Uuid,
        request_id: Uuid,
        override_content: Option<String>,
        trace_id: Option<String>,
    ) -> Result<MutationResult, MutationError> {
        let new_request_id = Uuid::new_v4();
        let new_turn_id = Uuid::new_v4();

        let turn_repo = Arc::clone(&self.turn_repo);
        let message_repo = Arc::clone(&self.message_repo);
        let chat_repo = Arc::clone(&self.chat_repo);
        let message_attachment_repo = Arc::clone(&self.message_attachment_repo);
        let outbox_enqueuer = Arc::clone(&self.outbox_enqueuer);
        let scope_tx = chat_scope.clone();
        let ctx_clone = ctx.clone();

        let (user_content, snapshot_boundary, chat_model) = self
            .db
            .transaction(|tx| {
                Box::pin(async move {
                    let (scope, target, chat_model) = validate_mutation(
                        &*chat_repo,
                        &*turn_repo,
                        &scope_tx,
                        &ctx_clone,
                        tx,
                        chat_id,
                        request_id,
                    )
                    .await
                    .map_err(mutation_to_db_err)?;

                    // Retrieve original user message for content (retry) / attachments (edit)
                    let original_msg = message_repo
                        .find_user_message_by_request_id(tx, &scope, chat_id, request_id)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?
                        .ok_or_else(|| {
                            modkit_db::DbError::Other(anyhow::anyhow!(
                                "User message not found for turn {request_id}"
                            ))
                        })?;

                    // Determine event type before consuming override_content.
                    let is_edit = override_content.is_some();
                    let user_content = override_content.unwrap_or(original_msg.content);

                    // Soft-delete old turn and its messages
                    turn_repo
                        .soft_delete(tx, &scope, target.id, Some(new_request_id))
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;
                    message_repo
                        .soft_delete_by_request_id(tx, &scope, chat_id, request_id)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    // Insert new running turn
                    let tenant_id = ctx_clone.subject_tenant_id();
                    let requester_type = ctx_clone.subject_type().unwrap_or("user").to_owned();

                    turn_repo
                        .create_turn(
                            tx,
                            &scope,
                            CreateTurnParams {
                                id: new_turn_id,
                                tenant_id,
                                chat_id,
                                request_id: new_request_id,
                                requester_type,
                                requester_user_id: Some(ctx_clone.subject_id()),
                                reserve_tokens: None,
                                max_output_tokens_applied: None,
                                reserved_credits_micro: None,
                                policy_version_applied: None,
                                effective_model: None,
                                minimal_generation_floor_applied: None,
                            },
                        )
                        .await
                        .map_err(|e| {
                            let err_str = e.to_string();
                            if err_str.contains("unique") || err_str.contains("UNIQUE") {
                                return mutation_to_db_err(MutationError::GenerationInProgress);
                            }
                            modkit_db::DbError::Other(anyhow::Error::new(e))
                        })?;

                    // Snapshot boundary: must be computed BEFORE inserting the new
                    // user message so context queries exclude it (DESIGN §ContextPlan P1).
                    let boundary = message_repo
                        .snapshot_boundary(tx, &scope, chat_id)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    // Insert user message for the new turn
                    let new_msg_id = Uuid::new_v4();
                    message_repo
                        .insert_user_message(
                            tx,
                            &scope,
                            InsertUserMessageParams {
                                id: new_msg_id,
                                tenant_id,
                                chat_id,
                                request_id: new_request_id,
                                content: user_content.clone(),
                            },
                        )
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    // Copy message_attachments from original message to new message,
                    // excluding soft-deleted attachments (P3-8).
                    message_attachment_repo
                        .copy_for_retry(tx, &scope, original_msg.id, new_msg_id, chat_id)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    // Enqueue audit event atomically within the same transaction.
                    let requester_type = requester_type_from_subject(&ctx_clone);
                    let audit_event = AuditEnvelope::Mutation(if is_edit {
                        TurnMutationAuditEvent::new_edit(
                            time::OffsetDateTime::now_utc(),
                            tenant_id,
                            requester_type,
                            trace_id,
                            ctx_clone.subject_id(),
                            chat_id,
                            target.id,
                            request_id,
                            new_request_id,
                        )
                    } else {
                        TurnMutationAuditEvent::new_retry(
                            time::OffsetDateTime::now_utc(),
                            tenant_id,
                            requester_type,
                            trace_id,
                            ctx_clone.subject_id(),
                            chat_id,
                            target.id,
                            request_id,
                            new_request_id,
                        )
                    });
                    outbox_enqueuer
                        .enqueue_audit_event(tx, audit_event)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    Ok((user_content, boundary, chat_model))
                })
            })
            .await
            .map_err(unwrap_mutation_err)?;

        Ok(MutationResult {
            new_request_id,
            new_turn_id,
            user_content,
            snapshot_boundary,
            chat_model,
        })
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Shared validation (5-check sequence) — free function for use in closures
// ════════════════════════════════════════════════════════════════════════════

async fn validate_mutation<CR: ChatRepository, TR: TurnRepository>(
    chat_repo: &CR,
    turn_repo: &TR,
    chat_scope: &AccessScope,
    ctx: &SecurityContext,
    tx: &impl modkit_db::secure::DBRunner,
    chat_id: Uuid,
    request_id: Uuid,
) -> Result<(AccessScope, TurnModel, String), MutationError> {
    // 1. Verify chat exists with pre-computed authorization scope
    let chat = chat_repo
        .get(tx, chat_scope, chat_id)
        .await
        .map_err(|e| MutationError::Internal {
            message: e.to_string(),
        })?
        .ok_or(MutationError::ChatNotFound { chat_id })?;
    let chat_model = chat.model;

    let scope = chat_scope.tenant_only();

    // 2. Acquire target turn by request_id
    let target = turn_repo
        .find_by_chat_and_request_id(tx, &scope, chat_id, request_id)
        .await
        .map_err(|e| MutationError::Internal {
            message: e.to_string(),
        })?
        .ok_or(MutationError::TurnNotFound {
            chat_id,
            request_id,
        })?;

    // 3. Verify ownership
    if target.requester_user_id != Some(ctx.subject_id()) {
        return Err(MutationError::Forbidden);
    }

    // 4. Verify terminal state
    if !target.state.is_terminal() {
        return Err(MutationError::InvalidTurnState {
            state: target.state.clone(),
        });
    }

    // 5. Verify latest turn (with FOR UPDATE for serialization)
    let latest = turn_repo
        .find_latest_for_update(tx, &scope, chat_id)
        .await
        .map_err(|e| MutationError::Internal {
            message: e.to_string(),
        })?;

    match latest {
        Some(ref l) if l.id == target.id => {} // target IS the latest — ok
        _ => return Err(MutationError::NotLatestTurn),
    }

    Ok((scope, target, chat_model))
}

// ════════════════════════════════════════════════════════════════════════════
// Error helpers for transaction boundary crossing
// ════════════════════════════════════════════════════════════════════════════

/// Map a mutation result to a label for the `result` metric dimension.
fn mutation_result_label<T>(result: &Result<T, MutationError>) -> &'static str {
    match result {
        Ok(_) => result_label::OK,
        Err(MutationError::NotLatestTurn) => result_label::NOT_LATEST,
        Err(MutationError::InvalidTurnState { .. }) => result_label::INVALID_STATE,
        Err(MutationError::Forbidden) => result_label::FORBIDDEN,
        Err(MutationError::GenerationInProgress) => result_label::GENERATION_IN_PROGRESS,
        Err(_) => result_label::ERROR,
    }
}

fn requester_type_from_subject(ctx: &SecurityContext) -> RequesterType {
    match ctx.subject_type() {
        Some("system") => RequesterType::System,
        _ => RequesterType::User,
    }
}

fn mutation_to_db_err(e: MutationError) -> modkit_db::DbError {
    modkit_db::DbError::Other(anyhow::Error::new(e))
}

fn unwrap_mutation_err(e: modkit_db::DbError) -> MutationError {
    match e {
        modkit_db::DbError::Other(anyhow_err) => match anyhow_err.downcast::<MutationError>() {
            Ok(me) => me,
            Err(other) => MutationError::Internal {
                message: other.to_string(),
            },
        },
        other => MutationError::Internal {
            message: other.to_string(),
        },
    }
}

#[cfg(test)]
#[path = "turn_service_test.rs"]
mod tests;
