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

use crate::domain::error::DomainError;
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
                match entity.as_str() {
                    "message" => MiniChatMessageError::not_found(detail)
                        .with_resource(resource)
                        .create(),
                    "turn" => MiniChatTurnError::not_found(detail)
                        .with_resource(resource)
                        .create(),
                    "attachment" => MiniChatAttachmentError::not_found(detail)
                        .with_resource(resource)
                        .create(),
                    _ => MiniChatChatError::not_found(detail)
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

            DomainError::Forbidden => MiniChatChatError::permission_denied()
                .with_reason("AUTHZ_DENIED")
                .create(),

            DomainError::Conflict { code, message } => MiniChatChatError::already_exists(message)
                .with_resource(code)
                .create(),

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

            DomainError::WebSearchDisabled => MiniChatChatError::failed_precondition()
                .with_precondition_violation(
                    "web_search",
                    "disabled via kill switch",
                    "FEATURE_DISABLED",
                )
                .create(),

            DomainError::WebSearchCallsExceeded => {
                MiniChatChatError::resource_exhausted("web search calls exceeded for this message")
                    .with_quota_violation("web_search_calls", "max calls exceeded")
                    .create()
            }

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

            DomainError::ServiceUnavailable { message } => {
                tracing::warn!(reason = %message, "mini-chat service unavailable");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(5)
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

            StreamError::Conflict { code, message } => MiniChatTurnError::aborted(message)
                .with_reason(code)
                .create(),

            StreamError::TurnCreationFailed { source } => {
                tracing::warn!(error = %source, "pre-stream turn creation failed");
                CanonicalError::from(source)
            }

            // The source `DomainError::Forbidden` carries no extra detail
            // worth preserving; map straight to the canonical AuthZ denial.
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
                    "selected model does not support image input",
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
        assert_eq!(p.status, 404);
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], CHAT_GTS);
        assert_eq!(p.context["resource_name"], id.to_string());
    }

    #[test]
    fn message_not_found_uses_message_resource_scope() {
        let id = Uuid::new_v4();
        let p: Problem = DomainError::MessageNotFound { id }.into_test_problem();
        assert_eq!(p.status, 404);
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
        assert_eq!(p.status, 404);
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], TURN_GTS);
        assert_eq!(p.context["resource_name"], request_id.to_string());
    }

    #[test]
    fn attachment_not_found_uses_attachment_resource_scope() {
        let id = Uuid::new_v4();
        let p: Problem = DomainError::NotFound {
            entity: "attachment".into(),
            id,
        }
        .into_test_problem();
        assert_eq!(p.status, 404);
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], ATTACHMENT_GTS);
        assert_eq!(p.context["resource_name"], id.to_string());
    }

    #[test]
    fn model_not_found_uses_model_resource_scope() {
        let p: Problem = DomainError::ModelNotFound {
            model_id: "gpt-fake".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, 404);
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_type"], MODEL_GTS);
        assert_eq!(p.context["resource_name"], "gpt-fake");
    }

    // ── Wire-status changes accepted by the migration plan ───────────────

    #[test]
    fn unsupported_file_type_now_maps_to_400() {
        // ⚠ wire change accepted in the migration plan: 415 → 400.
        let p: Problem = DomainError::UnsupportedFileType {
            mime: "application/x-msdownload".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, 400);
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
        assert_eq!(p.status, 400);
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
        assert_eq!(p.status, 400);
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
        assert_eq!(p.status, 503);
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
        assert_eq!(p.status, 400);
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
        assert_eq!(p.status, 400);
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
        assert_eq!(p.status, 429);
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
        assert_eq!(p.status, 429);
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
        assert_eq!(p.status, 403);
        assert_eq!(p.problem_type, PERMISSION_DENIED_TYPE);
        assert_eq!(p.context["reason"], "AUTHZ_DENIED");
    }

    #[test]
    fn validation_uses_format_variant_when_no_field_supplied() {
        let p: Problem = DomainError::Validation {
            message: "request is missing the required `content` field".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, 400);
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
    fn web_search_calls_exceeded_emits_quota_violation() {
        let p: Problem = DomainError::WebSearchCallsExceeded.into_test_problem();
        assert_eq!(p.status, 429);
        assert_eq!(p.problem_type, RESOURCE_EXHAUSTED_TYPE);
        let v = p
            .context
            .get("violations")
            .and_then(|v| v.as_array())
            .expect("violations must be present");
        assert_eq!(v[0]["subject"], "web_search_calls");
    }

    #[test]
    fn invalid_reaction_target_emits_precondition_violation() {
        let id = Uuid::new_v4();
        let p: Problem = DomainError::InvalidReactionTarget { id }.into_test_problem();
        assert_eq!(p.status, 400);
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
        assert_eq!(p.status, 400);
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
    fn service_unavailable_carries_retry_after_seconds() {
        let p: Problem = DomainError::ServiceUnavailable {
            message: "downstream timeout".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, 503);
        assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
        assert_eq!(p.context["retry_after_seconds"].as_u64(), Some(5));
    }

    #[test]
    fn invalid_model_emits_field_violation() {
        let p: Problem = DomainError::InvalidModel {
            model: "gpt-fake".into(),
        }
        .into_test_problem();
        assert_eq!(p.status, 400);
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
        assert_eq!(p.status, 409);
        assert_eq!(p.context["resource_name"], "unique_violation");
    }

    // ── MutationError / StreamError dedicated coverage ───────────────────

    #[test]
    fn mutation_not_latest_turn_emits_aborted_with_reason() {
        let p: Problem = MutationError::NotLatestTurn.into_test_problem();
        assert_eq!(p.status, 409);
        assert_eq!(p.problem_type, ABORTED_TYPE);
        assert_eq!(p.context["reason"], "NOT_LATEST_TURN");
    }

    #[test]
    fn mutation_generation_in_progress_emits_aborted_with_reason() {
        let p: Problem = MutationError::GenerationInProgress.into_test_problem();
        assert_eq!(p.status, 409);
        assert_eq!(p.problem_type, ABORTED_TYPE);
        assert_eq!(p.context["reason"], "GENERATION_IN_PROGRESS");
    }

    #[test]
    fn mutation_forbidden_emits_permission_denied_with_authz_reason() {
        let p: Problem = MutationError::Forbidden.into_test_problem();
        assert_eq!(p.status, 403);
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
        assert_eq!(p.status, 429);
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
        assert_eq!(p.status, 400);
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
