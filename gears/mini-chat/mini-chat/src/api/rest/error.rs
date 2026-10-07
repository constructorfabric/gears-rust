//! REST error mapping for the mini-chat gear.
//!
//! Maps domain-layer errors (`DomainError`, `MutationError`, `StreamError`)
//! to canonical errors (`toolkit-canonical-errors`) following the same pattern
//! used in `oagw` and `file-parser`. Provides `From<*>` for `CanonicalError`
//! — the long-lived mappings. Handlers return `ApiResult<T>`
//! (`= Result<T, CanonicalError>`); the canonical error middleware
//! (`toolkit::api::canonical_error_middleware`) converts `CanonicalError` to
//! a wire `Problem` and fills `instance` / `trace_id` post-response.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::{DomainError, NotFoundEntity};
use crate::domain::service::{MutationError, StreamError};

// ---------------------------------------------------------------------------
// Resource scopes
// ---------------------------------------------------------------------------

/// Errors attributable to a chat as a resource.
#[resource_error(gts_id!("cf.core.mini_chat.chat.v1~"))]
pub struct MiniChatChatError;

/// Errors attributable to a message as a resource.
#[resource_error(gts_id!("cf.core.mini_chat.message.v1~"))]
pub struct MiniChatMessageError;

/// Errors attributable to a turn as a resource.
#[resource_error(gts_id!("cf.core.mini_chat.turn.v1~"))]
pub struct MiniChatTurnError;

/// Errors attributable to an attachment as a resource.
#[resource_error(gts_id!("cf.core.mini_chat.attachment.v1~"))]
pub struct MiniChatAttachmentError;

/// Errors attributable to a model as a resource.
#[resource_error(gts_id!("cf.core.mini_chat.model.v1~"))]
pub struct MiniChatModelError;

// ---------------------------------------------------------------------------
// DomainError → CanonicalError
// ---------------------------------------------------------------------------

impl From<DomainError> for CanonicalError {
    #[allow(clippy::cognitive_complexity)]
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::ChatNotFound { id } => MiniChatChatError::not_found("Chat not found")
                .with_resource(id.to_string())
                .create(),

            DomainError::MessageNotFound { id } => {
                MiniChatMessageError::not_found("Message not found")
                    .with_resource(id.to_string())
                    .create()
            }

            DomainError::ModelNotFound { model_id } => {
                let detail = format!("Model '{model_id}' not found");
                MiniChatModelError::not_found(detail)
                    .with_resource(model_id)
                    .create()
            }

            DomainError::NotFound { entity, id } => {
                let detail = format!("{entity} not found: {id}");
                let resource = id.to_string();
                match entity {
                    NotFoundEntity::Attachment => MiniChatAttachmentError::not_found(detail)
                        .with_resource(resource)
                        .create(),
                    NotFoundEntity::Chat => MiniChatChatError::not_found(detail)
                        .with_resource(resource)
                        .create(),
                }
            }

            DomainError::InvalidModel { model } => MiniChatChatError::invalid_argument()
                .with_field_violation(
                    "model",
                    format!("Model '{model}' not in catalog"),
                    "INVALID_MODEL",
                )
                .create(),

            DomainError::Validation { message } => MiniChatChatError::invalid_argument()
                .with_format(message)
                .create(),

            // Same wire shape as `OData` errors raised by the query extractor.
            DomainError::OData(e) => CanonicalError::from(e),

            DomainError::Forbidden => MiniChatChatError::permission_denied()
                .with_reason("AUTHZ_DENIED")
                .create(),

            DomainError::AuthzUnavailable => authz_unavailable(),

            DomainError::Conflict { code, message } => {
                // `message` can carry driver constraint text or backend
                // names; it goes to the log, never to the client.
                tracing::warn!(conflict_code = %code, error_message = %message, "mini-chat conflict");
                MiniChatChatError::already_exists(conflict_detail(&code))
                    .with_resource(code)
                    .create()
            }

            DomainError::InvalidReactionTarget { id } => {
                MiniChatMessageError::failed_precondition()
                    .with_precondition_violation(
                        "reaction_target",
                        "message is not an assistant message",
                        "STATE",
                    )
                    .with_resource(id.to_string())
                    .create()
            }

            DomainError::Database { message } => {
                tracing::error!(error_message = %message, "mini-chat db error");
                CanonicalError::internal(message).create()
            }

            DomainError::InternalError { message } => {
                tracing::error!(error_message = %message, "mini-chat internal error");
                CanonicalError::internal(message).create()
            }

            DomainError::Outbox(e) => {
                use crate::domain::repos::OutboxError;
                let message = format!("{e}");
                match e {
                    // Caller-driven oversize is a bad request.
                    OutboxError::PayloadTooLarge { .. } => MiniChatChatError::invalid_argument()
                        .with_format(message)
                        .create(),
                    OutboxError::Serialize { .. } | OutboxError::Enqueue { .. } => {
                        tracing::error!(error_message = %message, "mini-chat outbox error");
                        CanonicalError::internal(message).create()
                    }
                }
            }

            DomainError::WebSearchDisabled => MiniChatChatError::failed_precondition()
                .with_precondition_violation(
                    "web_search",
                    "disabled via kill switch",
                    "FEATURE_DISABLED",
                )
                .create(),

            DomainError::ImagesDisabled => MiniChatChatError::failed_precondition()
                .with_precondition_violation(
                    "images",
                    "disabled via kill switch",
                    "FEATURE_DISABLED",
                )
                .create(),

            DomainError::InvalidTitle { message } => MiniChatChatError::invalid_argument()
                .with_field_violation("title", message, "INVALID_TITLE")
                .create(),

            DomainError::InvalidReaction => MiniChatChatError::invalid_argument()
                .with_field_violation(
                    "reaction",
                    "Reaction must be 'like' or 'dislike'",
                    "INVALID_REACTION",
                )
                .create(),

            DomainError::CodeInterpreterUnavailable => MiniChatAttachmentError::invalid_argument()
                .with_field_violation(
                    "file",
                    "Code interpreter is currently unavailable",
                    "CODE_INTERPRETER_UNAVAILABLE",
                )
                .create(),

            DomainError::UnsupportedFileType { mime } => {
                MiniChatAttachmentError::invalid_argument()
                    .with_field_violation(
                        "content_type",
                        format!("Unsupported file type: {mime}"),
                        "UNSUPPORTED_CONTENT_TYPE",
                    )
                    .create()
            }

            DomainError::FileTooLarge { message } => {
                MiniChatAttachmentError::out_of_range(message.clone())
                    .with_field_violation("content_length", message, "FILE_TOO_LARGE")
                    .create()
            }

            DomainError::DocumentLimitExceeded { message } => {
                MiniChatAttachmentError::resource_exhausted(message.clone())
                    .with_quota_violation("document_limit", message)
                    .create()
            }

            DomainError::StorageLimitExceeded { message } => {
                MiniChatAttachmentError::resource_exhausted(message.clone())
                    .with_quota_violation("storage_limit", message)
                    .create()
            }

            DomainError::ProviderError {
                code,
                sanitized_message,
            } => {
                tracing::error!(
                    provider_code = %code,
                    message = %sanitized_message,
                    "mini-chat provider error",
                );
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(10)
                    .create()
            }
        }
    }
}

/// Client-facing detail for a `DomainError::Conflict` code.
/// Seconds a client should wait before retrying after a PDP outage.
const AUTHZ_RETRY_AFTER_SECS: u64 = 5;

/// 503 for a PDP that could not evaluate the request: access is still
/// refused (fail closed), but the client sees a retryable outage, not a
/// denial. The detail is generic; the cause is only logged.
fn authz_unavailable() -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_retry_after_seconds(AUTHZ_RETRY_AFTER_SECS)
        .create()
}

fn conflict_detail(code: &str) -> &'static str {
    match code {
        "provider_mismatch" => "chat vector store belongs to another provider",
        "attachment_locked" => {
            "Attachment is referenced by one or more messages and cannot be deleted"
        }
        _ => "resource already exists",
    }
}

/// Client-facing detail for a `StreamError::Conflict` code.
fn turn_conflict_detail(code: &str) -> &'static str {
    match code {
        "turn_already_running" => "Another turn is running in this chat",
        "request_id_conflict" => "request_id is already used by another turn in this chat",
        _ => "Turn conflict",
    }
}

// ---------------------------------------------------------------------------
// MutationError → CanonicalError
// ---------------------------------------------------------------------------

impl From<MutationError> for CanonicalError {
    fn from(err: MutationError) -> Self {
        match err {
            MutationError::ChatNotFound { chat_id } => {
                MiniChatChatError::not_found("Chat not found")
                    .with_resource(chat_id.to_string())
                    .create()
            }

            MutationError::TurnNotFound { request_id, .. } => {
                MiniChatTurnError::not_found("Turn not found")
                    .with_resource(request_id.to_string())
                    .create()
            }

            MutationError::Forbidden => MiniChatTurnError::permission_denied()
                .with_reason("AUTHZ_DENIED")
                .create(),

            MutationError::AuthzUnavailable => authz_unavailable(),

            MutationError::InvalidTurnState { state } => MiniChatTurnError::failed_precondition()
                .with_precondition_violation(
                    "turn_state",
                    format!("turn is in {state:?} state"),
                    "STATE",
                )
                .create(),

            MutationError::NotLatestTurn => {
                MiniChatTurnError::aborted("Only the most recent turn can be mutated")
                    .with_reason("NOT_LATEST_TURN")
                    .create()
            }

            MutationError::GenerationInProgress => MiniChatTurnError::aborted(
                "Another generation is already in progress for this chat",
            )
            .with_reason("GENERATION_IN_PROGRESS")
            .create(),

            MutationError::Internal { message } => {
                tracing::warn!(error_message = %message, "turn mutation internal error");
                CanonicalError::internal(message).create()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// StreamError → CanonicalError
// ---------------------------------------------------------------------------

impl From<StreamError> for CanonicalError {
    fn from(err: StreamError) -> Self {
        match err {
            // Defensive only — handler intercepts Replay before reaching this
            // arm and serves the buffered SSE replay instead.
            StreamError::Replay { .. } => MiniChatTurnError::aborted("Duplicate request_id")
                .with_reason("REPLAY")
                .create(),

            StreamError::Conflict { code, message } => {
                // `message` names turn ids or carries driver constraint text;
                // it goes to the log, the client gets a fixed detail.
                tracing::info!(conflict_code = %code, error_message = %message, "turn conflict");
                MiniChatTurnError::aborted(turn_conflict_detail(&code))
                    .with_reason(code)
                    .create()
            }

            StreamError::TurnCreationFailed { source } => {
                tracing::warn!(error = %source, "pre-stream turn creation failed");
                CanonicalError::from(source)
            }

            // A PDP outage is 503; every other enforcer failure is the
            // canonical AuthZ denial (the source carries no extra detail).
            StreamError::AuthorizationFailed {
                source: DomainError::AuthzUnavailable,
            } => authz_unavailable(),
            StreamError::AuthorizationFailed { .. } => MiniChatChatError::permission_denied()
                .with_reason("AUTHZ_DENIED")
                .create(),

            StreamError::ChatNotFound { chat_id } => MiniChatChatError::not_found("Chat not found")
                .with_resource(chat_id.to_string())
                .create(),

            // `http_status` is dropped — canonical fixes status to 429 for
            // resource_exhausted regardless of upstream-supplied code.
            StreamError::QuotaExhausted {
                error_code,
                http_status: _,
                quota_scope,
            } => MiniChatChatError::resource_exhausted(format!("Quota '{quota_scope}' exhausted"))
                .with_quota_violation(quota_scope, error_code)
                .create(),

            StreamError::WebSearchDisabled => MiniChatChatError::failed_precondition()
                .with_precondition_violation(
                    "web_search",
                    "disabled via kill switch",
                    "FEATURE_DISABLED",
                )
                .create(),

            StreamError::ImagesDisabled => MiniChatChatError::failed_precondition()
                .with_precondition_violation(
                    "images",
                    "disabled via kill switch",
                    "FEATURE_DISABLED",
                )
                .create(),

            StreamError::TooManyImages { count, max } => MiniChatAttachmentError::out_of_range(
                format!("Request includes {count} images, max {max}"),
            )
            .with_field_violation("image_count", format!("{count}>{max}"), "TOO_MANY_IMAGES")
            .create(),

            StreamError::UnsupportedMedia => MiniChatAttachmentError::invalid_argument()
                .with_field_violation(
                    "content_type",
                    "the effective model does not support image input",
                    "VISION_NOT_SUPPORTED",
                )
                .create(),

            StreamError::InvalidAttachment { code, message } => {
                MiniChatAttachmentError::invalid_argument()
                    .with_field_violation("attachment", message, code)
                    .create()
            }

            StreamError::ContextBudgetExceeded {
                required_tokens,
                available_tokens,
            } => MiniChatChatError::out_of_range(format!(
                "Context requires {required_tokens} tokens but only {available_tokens} available"
            ))
            .with_field_violation(
                "context_tokens",
                format!("{required_tokens}>{available_tokens}"),
                "CONTEXT_BUDGET_EXCEEDED",
            )
            .create(),

            StreamError::InputTooLong {
                estimated_tokens,
                max_input_tokens,
            } => MiniChatChatError::out_of_range(format!(
                "Message too long: {estimated_tokens} tokens > max {max_input_tokens}"
            ))
            .with_field_violation(
                "input_tokens",
                format!("{estimated_tokens}>{max_input_tokens}"),
                "INPUT_TOO_LONG",
            )
            .create(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use toolkit_canonical_errors::Problem;
    use toolkit_gts::{gts_id, gts_uri};
    use uuid::Uuid;

    /// Build the wire `Problem` the canonical error middleware would emit
    /// for a given domain-layer error. Tests run without the middleware in
    /// scope, so `instance` / `trace_id` are never populated here — that
    /// injection is exercised by the integration tests that drive the full
    /// router.
    trait IntoTestProblem {
        fn into_test_problem(self) -> Problem;
    }

    impl<E> IntoTestProblem for E
    where
        CanonicalError: From<E>,
    {
        fn into_test_problem(self) -> Problem {
            Problem::from(CanonicalError::from(self))
        }
    }

    const NOT_FOUND_TYPE: &str = gts_uri!("cf.core.errors.err.v1~cf.core.err.not_found.v1~");
    const INVALID_ARGUMENT_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~");
    const OUT_OF_RANGE_TYPE: &str = gts_uri!("cf.core.errors.err.v1~cf.core.err.out_of_range.v1~");
    const RESOURCE_EXHAUSTED_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~");
    const FAILED_PRECONDITION_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~");
    const PERMISSION_DENIED_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.permission_denied.v1~");
    const ABORTED_TYPE: &str = gts_uri!("cf.core.errors.err.v1~cf.core.err.aborted.v1~");
    const SERVICE_UNAVAILABLE_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~");

    const CHAT_GTS: &str = gts_id!("cf.core.mini_chat.chat.v1~");
    const MESSAGE_GTS: &str = gts_id!("cf.core.mini_chat.message.v1~");
    const TURN_GTS: &str = gts_id!("cf.core.mini_chat.turn.v1~");
    const ATTACHMENT_GTS: &str = gts_id!("cf.core.mini_chat.attachment.v1~");
    const MODEL_GTS: &str = gts_id!("cf.core.mini_chat.model.v1~");

    // ── Resource scope coverage (one not_found per scope) ────────────────

    #[test]
    fn chat_not_found_uses_chat_resource_scope() {
        let id = Uuid::new_v4();
        let p: Problem = DomainError::ChatNotFound { id }.into_test_problem();
        assert_eq!(p.status, Some(404));
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], CHAT_GTS);
        assert_eq!(p.context["resource_name"], id.to_string());
    }

    #[test]
    fn message_not_found_uses_message_resource_scope() {
        let id = Uuid::new_v4();
        let p: Problem = DomainError::MessageNotFound { id }.into_test_problem();
        assert_eq!(p.status, Some(404));
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], MESSAGE_GTS);
        assert_eq!(p.context["resource_name"], id.to_string());
    }

    #[test]
    fn turn_not_found_uses_turn_resource_scope() {
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let p: Problem = MutationError::TurnNotFound {
            chat_id,
            request_id,
        }
        .into_test_problem();
        assert_eq!(p.status, Some(404));
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], TURN_GTS);
        assert_eq!(p.context["resource_name"], request_id.to_string());
    }

    #[test]
    fn attachment_not_found_uses_attachment_resource_scope() {
        let id = Uuid::new_v4();
        let p: Problem = DomainError::attachment_not_found(id).into_test_problem();
        assert_eq!(p.status, Some(404));
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], ATTACHMENT_GTS);
        assert_eq!(p.context["resource_name"], id.to_string());
        assert_eq!(p.detail, format!("Attachment not found: {id}"));
    }

    #[test]
    fn generic_chat_not_found_uses_chat_resource_scope() {
        let id = Uuid::new_v4();
        let p: Problem = DomainError::not_found(NotFoundEntity::Chat, id).into_test_problem();
        assert_eq!(p.status, Some(404));
        assert_eq!(p.context["resource_type"], CHAT_GTS);
        assert_eq!(p.context["resource_name"], id.to_string());
    }

    #[test]
    fn model_not_found_uses_model_resource_scope() {
        let p: Problem = DomainError::ModelNotFound {
            model_id: "gpt-fake".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(404));
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], MODEL_GTS);
        assert_eq!(p.context["resource_name"], "gpt-fake");
    }

    // ── Wire-status changes accepted by the migration plan ───────────────

    #[test]
    fn invalid_title_emits_field_violation() {
        let p: Problem = DomainError::InvalidTitle {
            message: "Title must be 255 characters or fewer".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        let v = p.context["field_violations"]
            .as_array()
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "title");
        assert_eq!(v[0]["reason"], "INVALID_TITLE");
    }

    #[test]
    fn invalid_reaction_emits_field_violation() {
        let p: Problem = DomainError::InvalidReaction.into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        let v = p.context["field_violations"]
            .as_array()
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "reaction");
        assert_eq!(v[0]["reason"], "INVALID_REACTION");
    }

    #[test]
    fn code_interpreter_unavailable_emits_field_violation() {
        let p: Problem = DomainError::CodeInterpreterUnavailable.into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        assert_eq!(p.context["resource_type"], ATTACHMENT_GTS);
        let v = p.context["field_violations"]
            .as_array()
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "file");
        assert_eq!(v[0]["reason"], "CODE_INTERPRETER_UNAVAILABLE");
    }

    #[test]
    fn unsupported_file_type_now_maps_to_400() {
        // ⚠ wire change accepted in the migration plan: 415 → 400.
        let p: Problem = DomainError::UnsupportedFileType {
            mime: "application/x-msdownload".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        assert_eq!(p.context["resource_type"], ATTACHMENT_GTS);
        let v = p
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "content_type");
        assert_eq!(v[0]["reason"], "UNSUPPORTED_CONTENT_TYPE");
    }

    #[test]
    fn unsupported_media_now_maps_to_400() {
        // ⚠ wire change accepted in the migration plan: 415 → 400.
        let p: Problem = StreamError::UnsupportedMedia.into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        let v = p
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "content_type");
        assert_eq!(v[0]["reason"], "VISION_NOT_SUPPORTED");
    }

    #[test]
    fn file_too_large_now_maps_to_400() {
        // ⚠ wire change accepted in the migration plan: 413 → 400.
        let p: Problem = DomainError::FileTooLarge {
            message: "file exceeds 10MB".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, OUT_OF_RANGE_TYPE);
        let v = p
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "content_length");
        assert_eq!(v[0]["reason"], "FILE_TOO_LARGE");
    }

    #[test]
    fn provider_error_now_maps_to_503() {
        // ⚠ wire change accepted in the migration plan: 502 → 503.
        let p: Problem = DomainError::ProviderError {
            code: "openai_error".into(),
            sanitized_message: "provider failure".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(503));
        assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
        assert_eq!(p.context["retry_after_seconds"].as_u64(), Some(10));
    }

    #[test]
    fn context_budget_exceeded_now_maps_to_400() {
        // ⚠ wire change accepted in the migration plan: 422 → 400.
        let p: Problem = StreamError::ContextBudgetExceeded {
            required_tokens: 5000,
            available_tokens: 4000,
        }
        .into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, OUT_OF_RANGE_TYPE);
        let v = p
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "context_tokens");
        assert_eq!(v[0]["reason"], "CONTEXT_BUDGET_EXCEEDED");
    }

    #[test]
    fn input_too_long_now_maps_to_400() {
        // ⚠ wire change accepted in the migration plan: 422 → 400.
        let p: Problem = StreamError::InputTooLong {
            estimated_tokens: 9000,
            max_input_tokens: 8000,
        }
        .into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, OUT_OF_RANGE_TYPE);
        let v = p
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "input_tokens");
        assert_eq!(v[0]["reason"], "INPUT_TOO_LONG");
    }

    #[test]
    fn document_limit_exceeded_now_maps_to_429() {
        // ⚠ wire change accepted in the migration plan: 400 → 429.
        let p: Problem = DomainError::DocumentLimitExceeded {
            message: "max 50 documents per chat".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(429));
        assert_eq!(p.problem_type, RESOURCE_EXHAUSTED_TYPE);
        let v = p
            .context
            .get("violations")
            .and_then(|v| v.as_array())
            .expect("violations must be present");
        assert_eq!(v[0]["subject"], "document_limit");
    }

    #[test]
    fn storage_limit_exceeded_now_maps_to_429() {
        // ⚠ wire change accepted in the migration plan: 400 → 429.
        let p: Problem = DomainError::StorageLimitExceeded {
            message: "tenant storage quota reached".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(429));
        assert_eq!(p.problem_type, RESOURCE_EXHAUSTED_TYPE);
        let v = p
            .context
            .get("violations")
            .and_then(|v| v.as_array())
            .expect("violations must be present");
        assert_eq!(v[0]["subject"], "storage_limit");
    }

    // ── Structured-context coverage ──────────────────────────────────────

    #[test]
    fn forbidden_carries_authz_denied_reason() {
        let p: Problem = DomainError::Forbidden.into_test_problem();
        assert_eq!(p.status, Some(403));
        assert_eq!(p.problem_type, PERMISSION_DENIED_TYPE);
        assert_eq!(p.context["reason"], "AUTHZ_DENIED");
    }

    /// A PDP outage is a retryable 503, not a 403 denial, on every error
    /// path that carries it.
    #[test]
    fn authz_unavailable_is_503_with_retry_after() {
        let problems = [
            DomainError::AuthzUnavailable.into_test_problem(),
            crate::domain::service::MutationError::AuthzUnavailable.into_test_problem(),
            crate::domain::service::StreamError::AuthorizationFailed {
                source: DomainError::AuthzUnavailable,
            }
            .into_test_problem(),
        ];
        for p in problems {
            assert_eq!(p.status, Some(503), "{p:?}");
            assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
            assert_eq!(p.context["retry_after_seconds"].as_u64(), Some(5));
        }
        // A policy denial through the stream path stays 403.
        let p = crate::domain::service::StreamError::AuthorizationFailed {
            source: DomainError::Forbidden,
        }
        .into_test_problem();
        assert_eq!(p.status, Some(403));
    }

    #[test]
    fn validation_uses_format_variant_when_no_field_supplied() {
        let p: Problem = DomainError::Validation {
            message: "request is missing the required `content` field".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        // Format variant — no field_violations array, message surfaces in `format`.
        assert!(
            p.context.get("field_violations").is_none()
                || p.context["field_violations"].as_array().unwrap().is_empty(),
            "expected no field_violations on Format variant, got {:?}",
            p.context,
        );
        assert!(
            p.context["format"]
                .as_str()
                .is_some_and(|s| s.contains("required `content` field")),
            "expected format string to carry the validation message, got {:?}",
            p.context,
        );
    }

    #[test]
    fn odata_errors_use_the_canonical_odata_mapping() {
        const ODATA_GTS: &str = gts_id!("cf.core.odata.query.v1~");
        for (err, field, reason) in [
            (
                toolkit_odata::Error::InvalidFilter("unknown field `nope`".into()),
                "$filter",
                "INVALID_FILTER",
            ),
            (
                toolkit_odata::Error::InvalidOrderByField("nope".into()),
                "$orderby",
                "INVALID_ORDERBY_FIELD",
            ),
            (
                toolkit_odata::Error::FilterMismatch,
                "cursor",
                "FILTER_MISMATCH",
            ),
            (
                toolkit_odata::Error::OrderMismatch,
                "cursor",
                "ORDER_MISMATCH",
            ),
            (
                toolkit_odata::Error::InvalidCursor,
                "cursor",
                "INVALID_CURSOR",
            ),
            (
                toolkit_odata::Error::CursorInvalidJson,
                "cursor",
                "INVALID_CURSOR",
            ),
            (toolkit_odata::Error::InvalidLimit, "$top", "INVALID_LIMIT"),
        ] {
            let p: Problem = DomainError::OData(err).into_test_problem();
            assert_eq!(p.status, Some(400), "{reason}");
            assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE, "{reason}");
            assert_eq!(p.context["resource_type"], ODATA_GTS, "{reason}");
            let v = p.context["field_violations"]
                .as_array()
                .expect("field_violations must be present");
            assert_eq!(v[0]["field"], field, "{reason}");
            assert_eq!(v[0]["reason"], reason);
        }
    }

    #[test]
    fn invalid_reaction_target_emits_precondition_violation() {
        let id = Uuid::new_v4();
        let p: Problem = DomainError::InvalidReactionTarget { id }.into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, FAILED_PRECONDITION_TYPE);
        let v = p
            .context
            .get("violations")
            .and_then(|v| v.as_array())
            .expect("violations must be present");
        assert_eq!(v[0]["subject"], "reaction_target");
        // PreconditionViolation field `type_` serializes as `type` on the wire.
        assert_eq!(v[0]["type"], "STATE");
        // The resource_id is preserved at the top level.
        assert_eq!(p.context["resource_type"], MESSAGE_GTS);
        assert_eq!(p.context["resource_name"], id.to_string());
    }

    #[test]
    fn web_search_disabled_emits_precondition_violation() {
        let p: Problem = DomainError::WebSearchDisabled.into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, FAILED_PRECONDITION_TYPE);
        let v = p
            .context
            .get("violations")
            .and_then(|v| v.as_array())
            .expect("violations must be present");
        assert_eq!(v[0]["subject"], "web_search");
        assert_eq!(v[0]["type"], "FEATURE_DISABLED");
    }

    #[test]
    fn images_disabled_emits_precondition_violation() {
        let p: Problem = DomainError::ImagesDisabled.into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, FAILED_PRECONDITION_TYPE);
        let v = p
            .context
            .get("violations")
            .and_then(|v| v.as_array())
            .expect("violations must be present");
        assert_eq!(v[0]["subject"], "images");
        assert_eq!(v[0]["type"], "FEATURE_DISABLED");
    }

    #[test]
    fn invalid_model_emits_field_violation() {
        let p: Problem = DomainError::InvalidModel {
            model: "gpt-fake".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        let v = p
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "model");
        assert_eq!(v[0]["reason"], "INVALID_MODEL");
    }

    #[test]
    fn conflict_emits_already_exists_with_resource() {
        let p: Problem = DomainError::Conflict {
            code: "unique_violation".into(),
            message: "alias already exists".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(409));
        assert_eq!(p.context["resource_name"], "unique_violation");
    }

    #[test]
    fn turn_conflict_detail_hides_raw_message() {
        let chat_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        for (code, raw, detail) in [
            (
                "turn_already_running",
                format!("Chat {chat_id} already has a running turn {turn_id}"),
                "Another turn is running in this chat",
            ),
            (
                "request_id_conflict",
                format!("Turn for request_id {turn_id} exists with state Running"),
                "request_id is already used by another turn in this chat",
            ),
            (
                "turn_already_running",
                "UNIQUE constraint failed: chat_turns.chat_id".to_owned(),
                "Another turn is running in this chat",
            ),
        ] {
            let p: Problem = StreamError::Conflict {
                code: code.into(),
                message: raw.clone(),
            }
            .into_test_problem();
            assert_eq!(p.status, Some(409), "{code}");
            assert_eq!(p.detail, detail, "{code}");
            assert_eq!(p.context["reason"], code);
            let wire = serde_json::to_string(&p).unwrap();
            assert!(!wire.contains(&raw), "{code}: raw message leaked: {wire}");
            assert!(
                !wire.contains(&turn_id.to_string()),
                "{code}: turn id leaked"
            );
        }
    }

    #[test]
    fn conflict_detail_hides_raw_message() {
        for (code, raw, detail) in [
            (
                "unique_violation",
                "duplicate key value violates unique constraint \"uq_chat_vector_stores_chat\"",
                "resource already exists",
            ),
            (
                "provider_mismatch",
                "vector store provider mismatch: existing='openai', current='azure_openai'",
                "chat vector store belongs to another provider",
            ),
            (
                "attachment_locked",
                "internal text",
                "Attachment is referenced by one or more messages and cannot be deleted",
            ),
            ("some_new_code", "internal text", "resource already exists"),
        ] {
            let p: Problem = DomainError::Conflict {
                code: code.into(),
                message: raw.into(),
            }
            .into_test_problem();
            assert_eq!(p.status, Some(409), "{code}");
            assert_eq!(p.detail, detail, "{code}");
            assert_eq!(p.context["resource_name"], code);
            let wire = serde_json::to_string(&p).unwrap();
            assert!(!wire.contains(raw), "{code}: raw message leaked: {wire}");
        }
    }

    // ── MutationError / StreamError dedicated coverage ───────────────────

    #[test]
    fn mutation_not_latest_turn_emits_aborted_with_reason() {
        let p: Problem = MutationError::NotLatestTurn.into_test_problem();
        assert_eq!(p.status, Some(409));
        assert_eq!(p.problem_type, ABORTED_TYPE);
        assert_eq!(p.context["reason"], "NOT_LATEST_TURN");
    }

    #[test]
    fn mutation_generation_in_progress_emits_aborted_with_reason() {
        let p: Problem = MutationError::GenerationInProgress.into_test_problem();
        assert_eq!(p.status, Some(409));
        assert_eq!(p.problem_type, ABORTED_TYPE);
        assert_eq!(p.context["reason"], "GENERATION_IN_PROGRESS");
    }

    #[test]
    fn mutation_forbidden_emits_permission_denied_with_authz_reason() {
        let p: Problem = MutationError::Forbidden.into_test_problem();
        assert_eq!(p.status, Some(403));
        assert_eq!(p.problem_type, PERMISSION_DENIED_TYPE);
        assert_eq!(p.context["resource_type"], TURN_GTS);
        assert_eq!(p.context["reason"], "AUTHZ_DENIED");
    }

    #[test]
    fn stream_quota_exhausted_emits_429_regardless_of_supplied_status() {
        // The upstream-supplied http_status is ignored — canonical fixes 429.
        let p: Problem = StreamError::QuotaExhausted {
            error_code: "RATE_LIMITED".into(),
            http_status: 503,
            quota_scope: "tokens".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, Some(429));
        assert_eq!(p.problem_type, RESOURCE_EXHAUSTED_TYPE);
        let v = p
            .context
            .get("violations")
            .and_then(|v| v.as_array())
            .expect("violations must be present");
        assert_eq!(v[0]["subject"], "tokens");
        assert_eq!(v[0]["description"], "RATE_LIMITED");
    }

    #[test]
    fn stream_too_many_images_emits_out_of_range_field_violation() {
        let p: Problem = StreamError::TooManyImages { count: 5, max: 3 }.into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(p.problem_type, OUT_OF_RANGE_TYPE);
        let v = p
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("field_violations must be present");
        assert_eq!(v[0]["field"], "image_count");
        assert_eq!(v[0]["reason"], "TOO_MANY_IMAGES");
    }

    #[test]
    fn instance_is_unset_at_canonical_layer() {
        // `instance` is filled by the canonical error middleware on the way
        // out — at the conversion layer (`From<DomainError> for
        // CanonicalError` → `Problem::from(canonical)`) it stays `None`
        // because no request URI is in scope.
        let p: Problem = DomainError::ChatNotFound { id: Uuid::nil() }.into_test_problem();
        assert!(p.instance.is_none());
    }
}
